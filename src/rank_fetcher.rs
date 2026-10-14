use crate::logging::{self, Event, Reason};
use crate::models::{
    Account, Freshness, RankFetchError, RankInfo, RankProvenance, RankedStats, RecentGame, TftRank,
};
use html_escape::decode_html_entities;
use regex::Regex;
use scraper::{Html, Selector};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

mod renewal;
use renewal::{freshness, now_ms, ProfileUpdate, RenewalState};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RefreshMode {
    #[default]
    Manual,
    Auto,
    /// Bypass our age threshold, but always respect OP.GG's renewableAt/429.
    Force,
}

/// Provider abstraction used by the manager and by deterministic tests.
pub trait RankProvider: Send + Sync {
    fn fetch_rank(&self, account: &Account) -> RankInfo;
    fn fetch_rank_with_mode(&self, account: &Account, _mode: RefreshMode) -> RankInfo {
        self.fetch_rank(account)
    }
}

#[derive(Clone)]
pub struct RankFetcher {
    client: reqwest::blocking::Client,
    renewal: Arc<RenewalState>,
}

impl Default for RankFetcher {
    fn default() -> Self {
        Self::new()
    }
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
        Self {
            client,
            renewal: Arc::default(),
        }
    }

    pub fn fetch_rank(&self, account: &Account) -> RankInfo {
        self.fetch_rank_with_mode(account, RefreshMode::Manual)
    }

    pub fn fetch_rank_with_mode(&self, account: &Account, mode: RefreshMode) -> RankInfo {
        if account.name.trim().is_empty() {
            return RankInfo::unranked();
        }
        let url = self.build_opgg_url(&account.region, &account.name);
        let tft_url = self.build_opgg_tft_url(&account.region, &account.name);
        let mut info = self.fetch_profiles(&url, &tft_url, &account.region, mode);
        // If a network request failed, provenance still describes the retained data.
        for (next, previous) in [
            (&mut info.provenance, &account.rank_provenance),
            (&mut info.tft_provenance, &account.tft_provenance),
        ] {
            if let (Some(next), Some(previous)) = (next.as_mut(), previous) {
                if next.fetched_at.is_none() {
                    next.fetched_at = previous.fetched_at;
                    next.source_updated_at = previous.source_updated_at;
                    next.freshness = Freshness::Stale;
                }
            }
        }
        info
    }

    fn fetch_profiles(
        &self,
        lol_url: &str,
        tft_url: &str,
        region: &str,
        mode: RefreshMode,
    ) -> RankInfo {
        let mut rank = self.fetch_rank_from_url(lol_url, region, mode);
        match self.fetch_tft_from_url(tft_url) {
            Ok(tft) => {
                rank.tft = Some(tft);
                rank.riot_id_not_found = Some(false);
                rank.tft_provenance = Some(RankProvenance {
                    source: "OP.GG".into(),
                    fetched_at: Some(now_ms()),
                    ..Default::default()
                });
            }
            Err(reason) => {
                rank.tft_provenance = Some(failed_provenance(reason));
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
                if !decoded_payload.contains("profile_icons/profileIcon")
                    || !decoded_payload.contains("\"entry\"")
                {
                    return Err(Reason::ProfileMissing);
                }
                self.parse_tft_profile(&decoded_payload)
            })
            .inspect_err(|reason| logging::record(Event::TftFetchFailed, *reason))
    }

    /// Parse the ranked TFT block of an OP.GG TFT profile payload.
    ///
    /// The page embeds the current set as the first `"entry":{...}` object and
    /// every earlier set as `{"setName":"TFTSetN","entry":{...}}`, newest first.
    pub fn parse_tft_from_opgg(&self, decoded_payload: &str) -> TftRank {
        self.parse_tft_profile(decoded_payload)
            .unwrap_or_else(|_| TftRank::error())
    }

    fn parse_tft_profile(&self, decoded_payload: &str) -> Result<TftRank, Reason> {
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

        if current.is_none() {
            return Err(Reason::ProfileMissing);
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
        Ok(rank)
    }

    pub fn build_opgg_tft_url(&self, region: &str, summoner_name: &str) -> String {
        self.build_opgg_url(region, summoner_name)
            .replacen("/lol/summoners/", "/tft/summoners/", 1)
    }

    fn fetch_rank_from_url(&self, url: &str, region: &str, mode: RefreshMode) -> RankInfo {
        self.fetch_from_opgg(url, region, mode)
            .map(|info| self.with_defaults(info))
            .unwrap_or_else(|reason| {
                logging::record(Event::RankFetchFailed, reason);
                RankInfo {
                    riot_id_not_found: (reason == Reason::HttpNotFound).then_some(true),
                    provenance: Some(failed_provenance(reason)),
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
        self.request_text(self.client.get(url))
    }

    fn request_text(&self, request: reqwest::blocking::RequestBuilder) -> Result<String, Reason> {
        if self.renewal.rate_limited() {
            return Err(Reason::HttpRateLimited);
        }
        let response = request
            .header(reqwest::header::ACCEPT_LANGUAGE, "en-US,en;q=0.9")
            .send()
            .map_err(|error| Reason::from_request(&error))?;
        if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            self.renewal.set_cooldown(
                response
                    .headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|value| value.to_str().ok())
                    .and_then(renewal::retry_after),
            );
        }
        let response = response
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

    fn fetch_from_opgg(
        &self,
        url: &str,
        region: &str,
        mode: RefreshMode,
    ) -> Result<RankInfo, Reason> {
        let mut body = self.fetch_page(url)?;
        let mut rank = self.parse_profile(&body)?;
        let decoded = decode_html_entities(&body).replace("\\\"", "\"");
        let profile = ProfileUpdate::parse(&decoded);
        let mut provenance = RankProvenance {
            source: "OP.GG".into(),
            fetched_at: Some(now_ms()),
            source_updated_at: profile.as_ref().and_then(|p| p.updated_at),
            freshness: freshness(profile.as_ref().and_then(|p| p.updated_at), mode, now_ms()),
            ..Default::default()
        };
        if let Some(profile) = profile.filter(|p| p.should_renew(mode, now_ms())) {
            provenance.renewal_attempted = true;
            match self.renew_profile(url, region, &body, &profile) {
                Ok(finished_at) => {
                    match self.fetch_page(url).and_then(|next| {
                        let rank = self.parse_profile(&next)?;
                        body = next;
                        Ok(rank)
                    }) {
                        Ok(next) => {
                            rank = next;
                            let decoded = decode_html_entities(&body).replace("\\\"", "\"");
                            provenance.fetched_at = Some(now_ms());
                            provenance.source_updated_at =
                                ProfileUpdate::parse(&decoded).and_then(|p| p.updated_at);
                            provenance.freshness =
                                freshness(provenance.source_updated_at, mode, now_ms());
                            provenance.renewal_succeeded = provenance.freshness == Freshness::Fresh
                                && provenance
                                    .source_updated_at
                                    .is_some_and(|at| at >= finished_at);
                            if !provenance.renewal_succeeded {
                                provenance.freshness = Freshness::Stale;
                                provenance.error = Some(RankFetchError::RenewalFailed);
                            }
                        }
                        Err(reason) => {
                            provenance.error = Some(fetch_error(reason));
                            provenance.freshness = Freshness::Stale;
                        }
                    }
                }
                Err(error) => {
                    provenance.error = Some(error);
                }
            }
        }
        if rank.recent_games.is_empty() {
            rank.recent_games = self
                .fetch_recent_games_from_opgg(&body, url, region)
                .unwrap_or_default();
        }
        rank.provenance = Some(provenance);
        Ok(rank)
    }

    fn fetch_recent_games_from_opgg(
        &self,
        body: &str,
        url: &str,
        region: &str,
    ) -> Result<Vec<RecentGame>, Reason> {
        let soup = Html::parse_document(body);
        let puuid = opgg_puuid(&soup).ok_or(Reason::ProfileMissing)?;
        let mut page = self.request_opgg_games(body, url, region, &puuid, "")?;
        if page.data.is_empty() {
            if let Some(cursor) = page
                .meta
                .get("last_game_created_at")
                .and_then(serde_json::Value::as_str)
            {
                page = self.request_opgg_games(body, url, region, &puuid, cursor)?;
            }
        }
        Ok(parse_opgg_action_games(&page.data))
    }

    fn request_opgg_games(
        &self,
        body: &str,
        url: &str,
        region: &str,
        puuid: &str,
        ended_at: &str,
    ) -> Result<OpggGamesPage, Reason> {
        let action = self
            .discover_get_games_action(body)
            .map_err(|_| Reason::ResponseBody)?;
        let payload = serde_json::json!([{
            "locale": "en",
            "region": region,
            "puuid": puuid,
            "gameType": "TOTAL",
            "endedAt": ended_at,
            "champion": serde_json::Value::Null,
        }]);
        let request = self
            .client
            .post(url)
            .header("Next-Action", &action)
            .header(reqwest::header::ACCEPT, "text/x-component")
            .header(reqwest::header::CONTENT_TYPE, "text/plain;charset=UTF-8")
            .header(reqwest::header::ORIGIN, "https://op.gg")
            .header(reqwest::header::REFERER, url)
            .body(payload.to_string());
        let response = self.request_text(request)?;
        match parse_opgg_action_page(&response) {
            Some(page) => Ok(page),
            None => {
                self.invalidate_get_games_action(&action);
                Err(Reason::ResponseBody)
            }
        }
    }

    /// Parse a fetched document independently of the network and renewal protocol.
    pub fn parse_profile(&self, body: &str) -> Result<RankInfo, Reason> {
        let decoded_payload = decode_html_entities(&body).replace("\\\"", "\"");
        if !decoded_payload.contains("profile_icons/profileIcon") {
            return Err(Reason::ProfileMissing);
        }

        let soup = Html::parse_document(body);
        let mut rank = self.parse_current_rank_from_opgg(&soup, &decoded_payload);
        rank.level = self.parse_level_from_opgg(&soup, &decoded_payload);
        rank.ranked_stats = meta_description(&soup).and_then(|text| parse_ranked_stats(&text));
        rank.recent_games = parse_recent_games(&soup, body);
        let explicitly_unranked = meta_description(&soup)
            .is_some_and(|text| text.contains(" / Lv. ") || text.contains(" / Unranked"));
        if rank.tier == "Unranked" && (rank.level.is_empty() || !explicitly_unranked) {
            return Err(Reason::ProfileMissing);
        }
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

        // OP.GG's Lv. description explicitly denotes an unranked profile.
        // Never use another player's or a historical tier_info for that profile.
        let explicitly_unranked = meta_description(soup)
            .is_some_and(|text| text.contains(" / Lv. ") || text.contains(" / Unranked"));
        if tier == "Unranked" && !explicitly_unranked {
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
            ranked_stats: rank.ranked_stats,
            recent_games: rank.recent_games,
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
            provenance: rank.provenance,
            tft_provenance: rank.tft_provenance,
        }
    }
}

impl RankProvider for RankFetcher {
    fn fetch_rank(&self, account: &Account) -> RankInfo {
        RankFetcher::fetch_rank(self, account)
    }
    fn fetch_rank_with_mode(&self, account: &Account, mode: RefreshMode) -> RankInfo {
        RankFetcher::fetch_rank_with_mode(self, account, mode)
    }
}

fn fetch_error(reason: Reason) -> RankFetchError {
    match reason {
        Reason::HttpNotFound => RankFetchError::ProfileNotFound,
        Reason::HttpRateLimited => RankFetchError::RateLimited,
        Reason::HttpForbidden | Reason::HttpUnauthorized => RankFetchError::Forbidden,
        Reason::Timeout => RankFetchError::Timeout,
        Reason::ProfileMissing | Reason::ResponseBody => RankFetchError::ParsingFailed,
        _ => RankFetchError::ProviderUnavailable,
    }
}

fn failed_provenance(reason: Reason) -> RankProvenance {
    RankProvenance {
        source: "OP.GG".into(),
        error: Some(fetch_error(reason)),
        ..Default::default()
    }
}

fn meta_description(soup: &Html) -> Option<String> {
    let selector = Selector::parse(r#"meta[name="description"]"#).ok()?;
    soup.select(&selector)
        .find_map(|element| element.value().attr("content").map(str::to_owned))
}

fn parse_ranked_stats(description: &str) -> Option<RankedStats> {
    static STATS: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)\b(?P<wins>[\d,]+)\s*Win\s+(?P<losses>[\d,]+)\s*Lose\s+Win rate\s+(?P<rate>\d{1,3})%")
            .expect("valid OP.GG ranked stats regex")
    });
    let captures = STATS.captures(description)?;
    let wins = captures
        .name("wins")?
        .as_str()
        .replace(',', "")
        .parse()
        .ok()?;
    let losses = captures
        .name("losses")?
        .as_str()
        .replace(',', "")
        .parse()
        .ok()?;
    let rate: u16 = captures.name("rate")?.as_str().parse().ok()?;
    (rate <= 100).then_some(RankedStats {
        wins,
        losses,
        win_rate: rate as u8,
    })
}

fn parse_recent_games(soup: &Html, body: &str) -> Vec<RecentGame> {
    static VERSION: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"meta/images/lol/(?P<version>\d+\.\d+\.\d+)/champion/")
            .expect("valid OP.GG champion asset version regex")
    });
    let Some(version) = VERSION
        .captures(body)
        .and_then(|capture| capture.name("version"))
        .map(|capture| capture.as_str())
    else {
        return Vec::new();
    };

    static JSON_LD: LazyLock<Selector> = LazyLock::new(|| {
        Selector::parse(r#"script[type="application/ld+json"]"#).expect("valid JSON-LD selector")
    });
    let items = soup.select(&JSON_LD).find_map(|script| {
        let source = script.text().collect::<String>();
        let document = serde_json::from_str::<serde_json::Value>(&source).ok()?;
        recent_games_list(&document).map(|items| items.to_vec())
    });
    let Some(items) = items else {
        return Vec::new();
    };

    items
        .iter()
        .take(10)
        .filter_map(|entry| {
            let game = entry.get("item")?;
            let properties = game.get("additionalProperty")?.as_array()?;
            let property = |name: &str| {
                properties
                    .iter()
                    .find(|property| property.get("name").and_then(serde_json::Value::as_str) == Some(name))
                    .and_then(|property| property.get("value"))
                    .and_then(serde_json::Value::as_str)
            };
            let champion = property("champion")?.trim();
            let won = match property("result")?.to_ascii_uppercase().as_str() {
                "WIN" => true,
                "LOSE" => false,
                _ => return None,
            };
            let icon_key = champion_asset_key(champion)?;
            Some(RecentGame {
                champion: champion.to_owned(),
                icon_url: format!(
                    "https://opgg-static.akamaized.net/meta/images/lol/{version}/champion/{icon_key}.png?image=c_crop,h_103,w_103,x_9,y_9/q_auto:good,f_webp,w_80,h_80"
                ),
                won,
            })
        })
        .collect()
}

struct OpggGamesPage {
    data: Vec<serde_json::Value>,
    meta: serde_json::Value,
}

fn opgg_puuid(soup: &Html) -> Option<String> {
    static JSON_LD: LazyLock<Selector> = LazyLock::new(|| {
        Selector::parse(r#"script[type="application/ld+json"]"#).expect("valid JSON-LD selector")
    });
    soup.select(&JSON_LD).find_map(|script| {
        let document =
            serde_json::from_str::<serde_json::Value>(&script.text().collect::<String>()).ok()?;
        find_opgg_puuid(&document)
    })
}

fn find_opgg_puuid(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::Object(object) => {
            if let Some(identifiers) = object
                .get("identifier")
                .and_then(serde_json::Value::as_array)
            {
                if let Some(puuid) = identifiers.iter().find_map(|identifier| {
                    (identifier.get("name").and_then(serde_json::Value::as_str) == Some("puuid"))
                        .then(|| identifier.get("value").and_then(serde_json::Value::as_str))
                        .flatten()
                }) {
                    return Some(puuid.to_owned());
                }
            }
            object.values().find_map(find_opgg_puuid)
        }
        serde_json::Value::Array(values) => values.iter().find_map(find_opgg_puuid),
        _ => None,
    }
}

fn parse_opgg_action_page(response: &str) -> Option<OpggGamesPage> {
    response.lines().find_map(|line| {
        let (_, json) = line.split_once(':')?;
        let value = serde_json::from_str::<serde_json::Value>(json).ok()?;
        Some(OpggGamesPage {
            data: value.get("data")?.as_array()?.to_vec(),
            meta: value.get("meta")?.clone(),
        })
    })
}

fn parse_opgg_action_games(data: &[serde_json::Value]) -> Vec<RecentGame> {
    data.iter()
        .filter_map(|game| {
            let champion = game.get("champion")?;
            let name = champion.get("name")?.as_str()?.trim();
            let won = match game
                .get("game_result")?
                .as_str()?
                .to_ascii_uppercase()
                .as_str()
            {
                "WIN" | "VICTORY" => true,
                "LOSE" | "DEFEAT" => false,
                _ => return None,
            };
            Some(RecentGame {
                champion: name.to_owned(),
                icon_url: champion.get("image_url")?.as_str()?.to_owned(),
                won,
            })
        })
        .take(10)
        .collect()
}

fn recent_games_list(value: &serde_json::Value) -> Option<&[serde_json::Value]> {
    match value {
        serde_json::Value::Object(object) => {
            let is_recent_games = object
                .get("@id")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|id| id.ends_with("#recent-games"));
            let is_item_list =
                object.get("@type").and_then(serde_json::Value::as_str) == Some("ItemList");
            if is_recent_games && is_item_list {
                return object
                    .get("itemListElement")
                    .and_then(serde_json::Value::as_array)
                    .map(Vec::as_slice);
            }
            object.values().find_map(recent_games_list)
        }
        serde_json::Value::Array(values) => values.iter().find_map(recent_games_list),
        _ => None,
    }
}

fn champion_asset_key(name: &str) -> Option<String> {
    let normalized = name.trim().to_ascii_lowercase();
    let known = match normalized.as_str() {
        "aurelion sol" => Some("AurelionSol"),
        "bel'veth" => Some("Belveth"),
        "cho'gath" => Some("Chogath"),
        "dr. mundo" => Some("DrMundo"),
        "jarvan iv" => Some("JarvanIV"),
        "kai'sa" => Some("Kaisa"),
        "kha'zix" => Some("Khazix"),
        "kog'maw" => Some("KogMaw"),
        "k'sante" => Some("KSante"),
        "leblanc" => Some("Leblanc"),
        "lee sin" => Some("LeeSin"),
        "miss fortune" => Some("MissFortune"),
        "nunu" | "nunu & willump" => Some("Nunu"),
        "renata glasc" => Some("Renata"),
        "rek'sai" => Some("RekSai"),
        "tahm kench" => Some("TahmKench"),
        "twisted fate" => Some("TwistedFate"),
        "vel'koz" => Some("Velkoz"),
        "wukong" | "monkey king" => Some("MonkeyKing"),
        "xin zhao" => Some("XinZhao"),
        _ => None,
    };
    if let Some(key) = known {
        return Some(key.to_owned());
    }

    let key = name
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_uppercase().collect::<String>() + chars.as_str()
            })
        })
        .collect::<String>();
    (!key.is_empty() && key.len() <= 32).then_some(key)
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
    fn reads_puuid_and_recent_game_records_from_opgg_action_payloads() {
        let html = r#"<script type="application/ld+json">{"@graph":[{"@type":"Person","identifier":[{"name":"region","value":"ru"},{"name":"puuid","value":"public-puuid"}]}]}</script>"#;
        let soup = Html::parse_document(html);
        assert_eq!(opgg_puuid(&soup).as_deref(), Some("public-puuid"));

        let response = concat!(
            "0:{\"a\":\"$@1\",\"f\":\"\",\"b\":\"test\"}\n",
            "1:{\"data\":[",
            "{\"game_result\":\"VICTORY\",\"champion\":{\"name\":\"Zeri\",\"image_url\":\"https://example.test/Zeri.png\"}},",
            "{\"game_result\":\"DEFEAT\",\"champion\":{\"name\":\"Varus\",\"image_url\":\"https://example.test/Varus.png\"}}",
            "],\"meta\":{\"first_game_created_at\":null,\"last_game_created_at\":\"2026-09-15T19:45:01+09:00\"}}"
        );
        let page = parse_opgg_action_page(response).unwrap();
        assert_eq!(
            page.meta["last_game_created_at"].as_str(),
            Some("2026-09-15T19:45:01+09:00")
        );
        assert_eq!(
            parse_opgg_action_games(&page.data),
            vec![
                RecentGame {
                    champion: "Zeri".into(),
                    icon_url: "https://example.test/Zeri.png".into(),
                    won: true,
                },
                RecentGame {
                    champion: "Varus".into(),
                    icon_url: "https://example.test/Varus.png".into(),
                    won: false,
                },
            ]
        );
    }

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
                    let body = r#"<meta name="description" content="Player / Lv. 83"><img src="profile_icons/profileIcon6.jpg">{"entry":{}}"#;
                    write!(stream, "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
                }
            }
        });
        let fetcher = RankFetcher {
            renewal: Arc::default(),
            client: reqwest::blocking::Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(5))
                .build()
                .unwrap(),
        };
        for (lol, tft, expected) in cases {
            let fetcher = RankFetcher {
                client: fetcher.client.clone(),
                renewal: Arc::default(),
            };
            assert_eq!(
                fetcher
                    .fetch_profiles(
                        &format!("http://{address}/lol"),
                        &format!("http://{address}/tft"),
                        "kr",
                        RefreshMode::Manual
                    )
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
            renewal: Arc::default(),
            client: reqwest::blocking::Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(5))
                .build()
                .unwrap(),
        };
        let url = format!("http://{address}/{account_id}?password={password}");
        let logs = logging::capture(|| {
            for _ in 0..4 {
                let fetcher = RankFetcher {
                    client: fetcher.client.clone(),
                    renewal: Arc::default(),
                };
                let info = fetcher.fetch_rank_from_url(&url, "kr", RefreshMode::Manual);
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
