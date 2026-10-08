use crate::logging::{self, Event, Reason};
use crate::models::{Account, RankInfo, TftRank};
use html_escape::decode_html_entities;
use regex::Regex;
use scraper::{Html, Selector};
use std::sync::LazyLock;
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
            .unwrap_or_else(|error| {
                logging::record(Event::RankClientFailed, Reason::from_request(&error));
                reqwest::blocking::Client::new()
            });
        Self { client }
    }

    pub fn fetch_rank(&self, account: &Account) -> RankInfo {
        if account.name.trim().is_empty() {
            return RankInfo::unranked();
        }
        let url = self.build_opgg_url(&account.region, &account.name);
        let tft_url = self.build_opgg_tft_url(&account.region, &account.name);
        self.fetch_profiles(&url, &tft_url)
    }

    fn fetch_profiles(&self, lol_url: &str, tft_url: &str) -> RankInfo {
        let mut rank = self.fetch_rank_from_url(lol_url);
        match self.fetch_tft_from_url(tft_url) {
            Ok(tft) => {
                rank.tft = Some(tft);
                rank.riot_id_not_found = Some(false);
            }
            Err(reason) => {
                if rank.riot_id_not_found == Some(true) && reason != Reason::HttpNotFound {
                    rank.riot_id_not_found = None;
                }
            }
        }
        rank
    }

    fn fetch_tft_from_url(&self, url: &str) -> Result<TftRank, Reason> {
        self.fetch_page(url)
            .and_then(|body| {
                let decoded_payload = decode_html_entities(&body).replace("\\\"", "\"");
                if !decoded_payload.contains("profile_icons/profileIcon") {
                    return Err(Reason::ProfileMissing);
                }
                Ok(self.parse_tft_from_opgg(&decoded_payload))
            })
            .inspect_err(|reason| logging::record(Event::TftFetchFailed, *reason))
    }

    /// Parse the ranked TFT block of an OP.GG TFT profile payload.
    ///
    /// The page embeds the current set as the first `"entry":{...}` object and
    /// every earlier set as `{"setName":"TFTSetN","entry":{...}}`, newest first.
    pub fn parse_tft_from_opgg(&self, decoded_payload: &str) -> TftRank {
        static ENTRY: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r#""entry"\s*:\s*"#).expect("valid TFT entry regex"));
        static SET_NAME: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(r#""setName"\s*:\s*"([^"]+)"\s*,\s*$"#).expect("valid TFT set regex")
        });
        let mut current: Option<serde_json::Value> = None;
        let mut current_set = None;
        let mut last_set: Option<serde_json::Value> = None;
        for found in ENTRY.find_iter(decoded_payload) {
            let Some(Ok(value)) =
                serde_json::Deserializer::from_str(&decoded_payload[found.end()..])
                    .into_iter::<serde_json::Value>()
                    .next()
            else {
                continue;
            };
            if !value.is_object() {
                continue;
            }
            let mut prefix_start = found.start().saturating_sub(80);
            while !decoded_payload.is_char_boundary(prefix_start) {
                prefix_start += 1;
            }
            let prefix = decoded_payload
                .get(prefix_start..found.start())
                .unwrap_or_default();
            match SET_NAME.captures(prefix) {
                None if current.is_none() => current = Some(value),
                Some(captures) if current.is_some() => {
                    let name = captures.get(1).map(|m| m.as_str().to_owned());
                    if current_set.is_none() {
                        current_set = current
                            .as_ref()
                            .and_then(|entry| entry["RANKED_TFT"]["tftSetCoreName"].as_str())
                            .map(str::to_owned);
                    }
                    // Skip the current set if it is repeated in the history list.
                    if name.is_some() && name == current_set {
                        continue;
                    }
                    last_set = Some(value);
                    break;
                }
                _ => {}
            }
        }

        let mut rank = TftRank::default();
        if let Some(ranked) = current.as_ref().map(|entry| &entry["RANKED_TFT"]) {
            if let Some(tier) = ranked["tier"].as_str() {
                let lp = ranked["leaguePoints"]
                    .as_i64()
                    .map(|lp| lp.to_string())
                    .unwrap_or_default();
                let (tier, division, lp) =
                    self.normalize_rank(tier, ranked["rank"].as_str().unwrap_or_default(), &lp);
                rank.tier = tier;
                rank.division = division;
                rank.lp = lp;
            }
        }
        rank.last_set = last_set
            .as_ref()
            .map(|entry| &entry["RANKED_TFT"])
            .and_then(|ranked| {
                let tier = ranked["tier"].as_str()?;
                let (tier, division, _) =
                    self.normalize_rank(tier, ranked["rank"].as_str().unwrap_or_default(), "");
                Some(if division.is_empty() {
                    tier
                } else {
                    format!("{tier} {division}")
                })
            })
            .unwrap_or_else(|| "Unranked".to_owned());
        rank
    }

    pub fn build_opgg_tft_url(&self, region: &str, summoner_name: &str) -> String {
        self.build_opgg_url(region, summoner_name)
            .replacen("/lol/summoners/", "/tft/summoners/", 1)
    }

    fn fetch_rank_from_url(&self, url: &str) -> RankInfo {
        self.fetch_from_opgg(url)
            .map(|info| self.with_defaults(info))
            .unwrap_or_else(|reason| {
                logging::record(Event::RankFetchFailed, reason);
                RankInfo {
                    riot_id_not_found: (reason == Reason::HttpNotFound).then_some(true),
                    ..RankInfo::error()
                }
            })
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

    fn fetch_page(&self, url: &str) -> Result<String, Reason> {
        let response = self
            .client
            .get(url)
            .header(reqwest::header::ACCEPT_LANGUAGE, "en-US,en;q=0.9")
            .send()
            .map_err(|error| Reason::from_request(&error))?
            .error_for_status()
            .map_err(|error| Reason::from_request(&error))?;
        response.text().map_err(|error| {
            if error.is_timeout() {
                Reason::Timeout
            } else {
                Reason::ResponseBody
            }
        })
    }

    fn fetch_from_opgg(&self, url: &str) -> Result<RankInfo, Reason> {
        let body = self.fetch_page(url)?;
        let decoded_payload = decode_html_entities(&body).replace("\\\"", "\"");
        if !decoded_payload.contains("profile_icons/profileIcon") {
            return Err(Reason::ProfileMissing);
        }

        let soup = Html::parse_document(&body);
        let mut rank = self.parse_current_rank_from_opgg(&soup, &decoded_payload);
        rank.level = self.parse_level_from_opgg(&soup, &decoded_payload);
        let (reached, finished) = self.parse_last_season_from_opgg(&decoded_payload);
        rank.reached_last_season = reached;
        rank.finished_last_season = finished;
        rank.riot_id_not_found = Some(false);
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
            static TIER_INFO: LazyLock<Regex> = LazyLock::new(|| {
                let pattern = r#""tier_info":\{"lp":(?P<lp>\d+),"tier":"(?P<tier>[A-Z_]+)","label":"(?P<label>[^"]+)"\}"#;
                Regex::new(pattern).expect("valid OP.GG tier regex")
            });
            if let Some(captures) = TIER_INFO.captures_iter(decoded_payload).last() {
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
            static LEVEL: LazyLock<Regex> =
                LazyLock::new(|| Regex::new(r"\bLv\.\s*(\d+)\b").expect("valid level regex"));
            if let Some(captures) = LEVEL.captures(&description) {
                return captures
                    .get(1)
                    .map(|value| value.as_str().to_owned())
                    .unwrap_or_default();
            }
        }

        static PROFILE_LEVEL: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(r"(?s)profile_icons/profileIcon\d+\.jpg.{0,1200}?<span[^>]*>(\d+)</span>")
                .expect("valid profile level regex")
        });
        if let Some(captures) = PROFILE_LEVEL.captures(decoded_payload) {
            return captures
                .get(1)
                .map(|value| value.as_str().to_owned())
                .unwrap_or_default();
        }

        static REACT_LEVEL: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(r#"(?s)profile_icons/profileIcon\d+\.jpg.{0,1600}?"children":(\d+)"#)
                .expect("valid react level regex")
        });
        REACT_LEVEL
            .captures(decoded_payload)
            .and_then(|captures| captures.get(1))
            .map(|value| value.as_str().to_owned())
            .unwrap_or_default()
    }

    pub fn parse_last_season_from_opgg(&self, decoded_payload: &str) -> (String, String) {
        static HISTORY: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r#""rank_entries"\s*:\s*"#).expect("valid history regex"));

        for entry in HISTORY.find_iter(decoded_payload) {
            // Parse one complete season object from the surrounding React payload.
            // Field order and additional rank metadata must not affect extraction.
            let Some(Ok(entries)) =
                serde_json::Deserializer::from_str(&decoded_payload[entry.end()..])
                    .into_iter::<serde_json::Value>()
                    .next()
            else {
                continue;
            };
            let format_rank = |key: &str| {
                let tier = entries[key]["tier"].as_str().unwrap_or_default();
                let lp = match &entries[key]["lp"] {
                    serde_json::Value::String(value) => value.clone(),
                    serde_json::Value::Number(value) => value.to_string(),
                    _ => String::new(),
                };
                self.format_history_rank(tier, &self.clean_lp_value(&lp))
            };
            let high_rank = format_rank("high_rank_info");
            let finished_rank = format_rank("rank_info");
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
        static RANK: LazyLock<Regex> = LazyLock::new(|| {
            let pattern = r"(?i)\b(?P<tier>Challenger|Grandmaster|Master|Diamond|Emerald|Platinum|Gold|Silver|Bronze|Iron)\s*(?P<division>IV|III|II|I|[1-4])?\s+(?P<lp>[\d,]+)\s*LP\b";
            Regex::new(pattern).expect("valid rank regex")
        });
        let captures = RANK.captures(text)?;
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
        static DIVISION: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"\b[A-Z]+\s+([1-4])\b").expect("valid division regex"));
        DIVISION
            .captures(label)
            .and_then(|captures| captures.get(1))
            .map(|value| value.as_str().to_owned())
            .unwrap_or_default()
    }

    pub fn format_history_rank(&self, tier_text: &str, lp: &str) -> String {
        let tier_text = tier_text.trim();
        if tier_text.is_empty() {
            return String::new();
        }
        static RANK: LazyLock<Regex> = LazyLock::new(|| {
            let pattern = r"(?i)^(?P<tier>Challenger|Grandmaster|Master|Diamond|Emerald|Platinum|Gold|Silver|Bronze|Iron)\s*(?P<division>IV|III|II|I|[1-4])?";
            Regex::new(pattern).expect("valid history rank regex")
        });
        let Some(captures) = RANK.captures(tier_text) else {
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
            riot_id_not_found: rank.riot_id_not_found,
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
            tft: rank.tft,
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

    #[test]
    fn missing_riot_id_requires_both_profiles_to_be_not_found() {
        use std::io::{BufRead, Write};
        let cases = [
            (404, 404, Some(true)),
            (403, 404, None),
            (404, 429, None),
            (503, 503, None),
            (404, 200, Some(false)),
            (200, 404, Some(false)),
            (200, 200, Some(false)),
        ];
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            for (lol, tft, _) in cases {
                for status in [lol, tft] {
                    let (mut stream, _) = listener.accept().unwrap();
                    stream
                        .set_read_timeout(Some(Duration::from_secs(5)))
                        .unwrap();
                    let mut reader = std::io::BufReader::new(&mut stream);
                    let mut header = String::new();
                    loop {
                        header.clear();
                        assert!(reader.read_line(&mut header).unwrap() > 0);
                        if header == "\r\n" {
                            break;
                        }
                    }
                    let body = "<img src='profile_icons/profileIcon6.jpg'>";
                    write!(stream, "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
                }
            }
        });
        let fetcher = RankFetcher {
            client: reqwest::blocking::Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(5))
                .build()
                .unwrap(),
        };
        for (lol, tft, expected) in cases {
            assert_eq!(
                fetcher
                    .fetch_profiles(&format!("http://{address}/lol"), &format!("http://{address}/tft"))
                    .riot_id_not_found,
                expected,
                "LoL={lol}, TFT={tft}"
            );
        }
        server.join().unwrap();
    }

    #[test]
    fn http_failures_log_only_categories_without_urls_or_bodies() {
        use std::io::{BufRead, Write};
        let account_id = "PRIVATE_ACCOUNT_ID_c514";
        let password = "PRIVATE_PASSWORD_6d92";
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            for status in [403, 429, 503, 200] {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut reader = std::io::BufReader::new(&mut stream);
                let mut header = String::new();
                loop {
                    header.clear();
                    assert!(reader.read_line(&mut header).unwrap() > 0);
                    if header == "\r\n" {
                        break;
                    }
                }
                let body = format!("{account_id}\n{password}");
                write!(stream, "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            }
        });
        let fetcher = RankFetcher {
            client: reqwest::blocking::Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(5))
                .build()
                .unwrap(),
        };
        let url = format!("http://{address}/{account_id}?password={password}");
        let logs = logging::capture(|| {
            for _ in 0..4 {
                let info = fetcher.fetch_rank_from_url(&url);
                assert_eq!(info.tier, "Error");
                assert_eq!(info.riot_id_not_found, None);
            }
        });
        server.join().unwrap();
        for reason in [
            "http_forbidden",
            "http_rate_limited",
            "http_server",
            "profile_missing",
        ] {
            assert!(logs.contains(&format!("event=rank_fetch_failed reason={reason}")));
        }
        for secret in [account_id, password, &url] {
            assert!(!logs.contains(secret));
        }
    }
}
