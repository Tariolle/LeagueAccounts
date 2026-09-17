use crate::models::{Account, RankInfo};
use html_escape::decode_html_entities;
use regex::Regex;
use scraper::{Html, Selector};
use std::time::Duration;

/// Provider abstraction used by the manager and by deterministic tests.
pub trait RankProvider: Send + Sync {
    fn fetch_rank(&self, account: &Account) -> RankInfo;
}

#[derive(Clone, Default)]
pub struct RankFetcher {
    client: reqwest::blocking::Client,
}

impl RankFetcher {
    pub fn new() -> Self {
        let client = reqwest::blocking::Client::builder()
            .user_agent(
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                 (KHTML, like Gecko) Chrome/125.0.0.0 Safari/537.36",
            )
            .timeout(Duration::from_secs(20))
            .build()
            .unwrap_or_else(|_| reqwest::blocking::Client::new());
        Self { client }
    }

    pub fn fetch_rank(&self, account: &Account) -> RankInfo {
        self.fetch_from_opgg(account)
            .map(|info| self.with_defaults(info))
            .unwrap_or_else(|_| RankInfo::error())
    }

    pub fn build_opgg_url(&self, region: &str, summoner_name: &str) -> String {
        let (game_name, tagline) = Self::split_riot_id(summoner_name);
        let mut formatted_name = urlencoding::encode(&game_name).into_owned();
        if !tagline.is_empty() {
            formatted_name.push('-');
            formatted_name.push_str(&urlencoding::encode(&tagline));
        }
        format!("https://op.gg/lol/summoners/{region}/{formatted_name}")
    }

    pub fn split_riot_id(summoner_name: &str) -> (String, String) {
        match summoner_name.rsplit_once('#') {
            Some((game_name, tagline)) => (game_name.trim().to_owned(), tagline.trim().to_owned()),
            None => (summoner_name.trim().to_owned(), String::new()),
        }
    }

    fn fetch_from_opgg(&self, account: &Account) -> Result<RankInfo, String> {
        let url = self.build_opgg_url(&account.region, &account.name);
        let response = self
            .client
            .get(url)
            .header(reqwest::header::ACCEPT_LANGUAGE, "en-US,en;q=0.9")
            .send()
            .map_err(|error| error.to_string())?
            .error_for_status()
            .map_err(|error| error.to_string())?;
        let body = response.text().map_err(|error| error.to_string())?;
        let decoded_payload = decode_html_entities(&body).replace("\\\"", "\"");
        if !decoded_payload.contains("profile_icons/profileIcon") {
            return Err("OP.GG profile payload was not found".to_owned());
        }

        let soup = Html::parse_document(&body);
        let mut rank = self.parse_current_rank_from_opgg(&soup, &decoded_payload);
        rank.level = self.parse_level_from_opgg(&soup, &decoded_payload);
        let (reached, finished) = self.parse_last_season_from_opgg(&decoded_payload);
        rank.reached_last_season = reached;
        rank.finished_last_season = finished;
        Ok(rank)
    }

    pub fn parse_current_rank_from_opgg(&self, soup: &Html, decoded_payload: &str) -> RankInfo {
        let mut tier = "Unranked".to_owned();
        let mut division = String::new();
        let mut lp = String::new();

        if let Some(description) = meta_description(soup) {
            if let Some((parsed_tier, parsed_division, parsed_lp)) =
                self.parse_rank_text(&description)
            {
                tier = parsed_tier;
                division = parsed_division;
                lp = parsed_lp;
            }
        }

        if tier == "Unranked" {
            let re = Regex::new(
                r#""tier_info":\{"lp":(?P<lp>\d+),"tier":"(?P<tier>[A-Z_]+)","label":"(?P<label>[^"]+)"\}"#,
            )
            .expect("valid OP.GG tier regex");
            if let Some(captures) = re.captures_iter(decoded_payload).last() {
                let label = captures
                    .name("label")
                    .map(|value| value.as_str())
                    .unwrap_or_default();
                let (normalized_tier, normalized_division, normalized_lp) = self.normalize_rank(
                    captures
                        .name("tier")
                        .map(|value| value.as_str())
                        .unwrap_or_default(),
                    &self.division_from_label(label),
                    captures
                        .name("lp")
                        .map(|value| value.as_str())
                        .unwrap_or_default(),
                );
                tier = normalized_tier;
                division = normalized_division;
                lp = normalized_lp;
            }
        }

        RankInfo {
            tier,
            division,
            lp,
            ..RankInfo::default()
        }
    }

    pub fn parse_level_from_opgg(&self, soup: &Html, decoded_payload: &str) -> String {
        if let Some(description) = meta_description(soup) {
            let re = Regex::new(r"\bLv\.\s*(\d+)\b").expect("valid level regex");
            if let Some(captures) = re.captures(&description) {
                return captures
                    .get(1)
                    .map(|value| value.as_str().to_owned())
                    .unwrap_or_default();
            }
        }

        let profile_level =
            Regex::new(r"profile_icons/profileIcon\d+\.jpg.{0,1200}?<span[^>]*>(\d+)</span>")
                .expect("valid profile level regex");
        if let Some(captures) = profile_level.captures(decoded_payload) {
            return captures
                .get(1)
                .map(|value| value.as_str().to_owned())
                .unwrap_or_default();
        }

        let react_level =
            Regex::new(r#"profile_icons/profileIcon\d+\.jpg.{0,1600}?"children":(\d+)"#)
                .expect("valid react level regex");
        react_level
            .captures(decoded_payload)
            .and_then(|captures| captures.get(1))
            .map(|value| value.as_str().to_owned())
            .unwrap_or_default()
    }

    pub fn parse_last_season_from_opgg(&self, decoded_payload: &str) -> (String, String) {
        let history = Regex::new(
            r#""season":"(?P<season>[^"]+)","rank_entries":\{"high_rank_info":\{"tier":"(?P<high_tier>[^"]*)","lp":(?P<high_lp>null|"[^"]*").*?\},"rank_info":\{"tier":"(?P<rank_tier>[^"]*)","lp":(?P<rank_lp>null|"[^"]*)""#,
        )
        .expect("valid history regex");

        for captures in history.captures_iter(decoded_payload) {
            let high_rank = self.format_history_rank(
                captures
                    .name("high_tier")
                    .map(|value| value.as_str())
                    .unwrap_or_default(),
                &self.clean_lp_value(
                    captures
                        .name("high_lp")
                        .map(|value| value.as_str())
                        .unwrap_or_default(),
                ),
            );
            let finished_rank = self.format_history_rank(
                captures
                    .name("rank_tier")
                    .map(|value| value.as_str())
                    .unwrap_or_default(),
                &self.clean_lp_value(
                    captures
                        .name("rank_lp")
                        .map(|value| value.as_str())
                        .unwrap_or_default(),
                ),
            );
            if !high_rank.is_empty() || !finished_rank.is_empty() {
                return (
                    if high_rank.is_empty() {
                        finished_rank.clone()
                    } else {
                        high_rank.clone()
                    },
                    if finished_rank.is_empty() {
                        high_rank
                    } else {
                        finished_rank
                    },
                );
            }
        }
        ("Unranked".to_owned(), "Unranked".to_owned())
    }

    pub fn parse_rank_text(&self, text: &str) -> Option<(String, String, String)> {
        let rank = Regex::new(
            r"(?i)\b(?P<tier>Challenger|Grandmaster|Master|Diamond|Emerald|Platinum|Gold|Silver|Bronze|Iron)\s*(?P<division>IV|III|II|I|[1-4])?\s+(?P<lp>[\d,]+)\s*LP\b",
        )
        .expect("valid rank regex");
        let captures = rank.captures(text)?;
        Some(
            self.normalize_rank(
                captures.name("tier")?.as_str(),
                captures
                    .name("division")
                    .map(|value| value.as_str())
                    .unwrap_or_default(),
                captures.name("lp")?.as_str(),
            ),
        )
    }

    pub fn normalize_rank(&self, tier: &str, division: &str, lp: &str) -> (String, String, String) {
        let normalized_tier = tier
            .replace('_', " ")
            .split_whitespace()
            .map(|part| {
                let mut chars = part.chars();
                match chars.next() {
                    Some(first) => {
                        first.to_uppercase().collect::<String>()
                            + &chars.as_str().to_ascii_lowercase()
                    }
                    None => String::new(),
                }
            })
            .collect::<String>();
        let normalized_tier = if normalized_tier.eq_ignore_ascii_case("grandmaster") {
            "Grandmaster".to_owned()
        } else {
            normalized_tier
        };
        let division_upper = division.trim().to_ascii_uppercase();
        let normalized_division = match division_upper.as_str() {
            "1" => "I",
            "2" => "II",
            "3" => "III",
            "4" => "IV",
            other => other,
        };
        let normalized_division = if matches!(
            normalized_tier.as_str(),
            "Challenger" | "Grandmaster" | "Master"
        ) {
            String::new()
        } else {
            normalized_division.to_owned()
        };
        (
            normalized_tier,
            normalized_division,
            lp.replace(',', "").trim().to_owned(),
        )
    }

    pub fn division_from_label(&self, label: &str) -> String {
        let re = Regex::new(r"\b[A-Z]+\s+([1-4])\b").expect("valid division regex");
        re.captures(label)
            .and_then(|captures| captures.get(1))
            .map(|value| value.as_str().to_owned())
            .unwrap_or_default()
    }

    pub fn format_history_rank(&self, tier_text: &str, lp: &str) -> String {
        let tier_text = tier_text.trim();
        if tier_text.is_empty() {
            return String::new();
        }
        let rank = Regex::new(
            r"(?i)^(?P<tier>Challenger|Grandmaster|Master|Diamond|Emerald|Platinum|Gold|Silver|Bronze|Iron)\s*(?P<division>IV|III|II|I|[1-4])?",
        )
        .expect("valid history rank regex");
        let Some(captures) = rank.captures(tier_text) else {
            return title_case(tier_text);
        };
        let (tier, division, _) = self.normalize_rank(
            captures
                .name("tier")
                .map(|value| value.as_str())
                .unwrap_or_default(),
            captures
                .name("division")
                .map(|value| value.as_str())
                .unwrap_or_default(),
            lp,
        );
        let mut parts = vec![tier];
        if !division.is_empty() {
            parts.push(division);
        }
        if !lp.is_empty() {
            parts.push(format!("{lp}LP"));
        }
        parts.join(" ")
    }

    pub fn clean_lp_value(&self, value: &str) -> String {
        let value = value.trim();
        if value == "null" || value.is_empty() {
            String::new()
        } else {
            value.trim_matches('"').replace(',', "")
        }
    }

    fn with_defaults(&self, rank: RankInfo) -> RankInfo {
        RankInfo {
            tier: if rank.tier.is_empty() {
                "Unranked".to_owned()
            } else {
                rank.tier
            },
            division: rank.division,
            lp: rank.lp,
            level: rank.level,
            reached_last_season: if rank.reached_last_season.is_empty() {
                "Unranked".to_owned()
            } else {
                rank.reached_last_season
            },
            finished_last_season: if rank.finished_last_season.is_empty() {
                "Unranked".to_owned()
            } else {
                rank.finished_last_season
            },
        }
    }
}

impl RankProvider for RankFetcher {
    fn fetch_rank(&self, account: &Account) -> RankInfo {
        RankFetcher::fetch_rank(self, account)
    }
}

fn meta_description(soup: &Html) -> Option<String> {
    let selector = Selector::parse(r#"meta[name="description"]"#).ok()?;
    soup.select(&selector)
        .find_map(|element| element.value().attr("content").map(str::to_owned))
}

fn title_case(value: &str) -> String {
    value
        .split_whitespace()
        .map(|part| {
            let mut chars = part.chars();
            chars
                .next()
                .map(|first| {
                    first.to_uppercase().collect::<String>() + &chars.as_str().to_ascii_lowercase()
                })
                .unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_current_rank_and_level() {
        let html_doc = r#"
        <html><head><meta name="description" content="Hide on bush#KR1 / Challenger 1 1952LP / 248Win 186Lose Win rate 57%"/></head>
        <body><img src="https://opgg-static.akamaized.net/meta/images/profile_icons/profileIcon6.jpg"/><div><span>909</span></div></body></html>
        "#;
        let soup = Html::parse_document(html_doc);
        let fetcher = RankFetcher::new();
        assert_eq!(
            fetcher.parse_current_rank_from_opgg(&soup, html_doc),
            RankInfo {
                tier: "Challenger".into(),
                division: String::new(),
                lp: "1952".into(),
                ..RankInfo::default()
            }
        );
        assert_eq!(fetcher.parse_level_from_opgg(&soup, html_doc), "909");
    }

    #[test]
    fn parses_unranked_level_from_description() {
        let soup =
            Html::parse_document(r#"<meta name="description" content="Faker#T 1 / Lv. 71"/>"#);
        let fetcher = RankFetcher::new();
        assert_eq!(
            fetcher.parse_current_rank_from_opgg(&soup, ""),
            RankInfo {
                tier: "Unranked".into(),
                ..RankInfo::default()
            }
        );
        assert_eq!(fetcher.parse_level_from_opgg(&soup, ""), "71");
    }

    #[test]
    fn parses_last_season_history() {
        let payload = r#""season":"S2025 ","rank_entries":{"high_rank_info":{"tier":"challenger","lp":"1,255","tier_image_url":"","tier_mini_image_url":""},"rank_info":{"tier":"master","lp":"285","tier_image_url":"","tier_mini_image_url":""}}"#;
        let fetcher = RankFetcher::new();
        assert_eq!(
            fetcher.parse_last_season_from_opgg(payload),
            ("Challenger 1255LP".into(), "Master 285LP".into())
        );
    }
}
