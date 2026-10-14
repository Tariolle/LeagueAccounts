use serde::{Deserialize, Serialize};

/// One League account tracked by the application.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Account {
    pub account_id: String,
    pub name: String,
    /// Set only when both OP.GG profiles report that the saved Riot ID is missing.
    #[serde(default)]
    pub riot_id_not_found: bool,
    pub region: String,
    pub region_display: String,
    /// Passwords are never written to the normal accounts file. They are kept
    /// in Windows Credential Manager and only held here while the app runs.
    #[serde(skip_serializing, default)]
    pub password: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "default_tier")]
    pub tier: String,
    #[serde(default)]
    pub division: String,
    #[serde(default)]
    pub lp: String,
    #[serde(default)]
    pub level: String,
    /// Ranked Solo/Duo wins and losses from the OP.GG profile description.
    #[serde(default)]
    pub ranked_stats: Option<RankedStats>,
    /// Recent LoL games in source order, newest first.
    #[serde(default)]
    pub recent_games: Vec<RecentGame>,
    #[serde(default = "default_history")]
    pub reached_last_season: String,
    #[serde(default = "default_history")]
    pub finished_last_season: String,
    /// Teamfight Tactics ranked standing for the same Riot account.
    #[serde(default)]
    pub tft: TftRank,
    /// LoL source freshness; TFT has its own independent fetch state.
    #[serde(default)]
    pub rank_provenance: Option<RankProvenance>,
    #[serde(default)]
    pub tft_provenance: Option<RankProvenance>,
}

/// Ranked TFT standing: the current set and the previous set's final rank.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct TftRank {
    #[serde(default = "default_tier")]
    pub tier: String,
    #[serde(default)]
    pub division: String,
    #[serde(default)]
    pub lp: String,
    #[serde(default = "default_history")]
    pub last_set: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RankedStats {
    pub wins: u32,
    pub losses: u32,
    pub win_rate: u8,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RecentGame {
    pub champion: String,
    pub icon_url: String,
    pub won: bool,
}

impl Default for TftRank {
    fn default() -> Self {
        Self {
            tier: default_tier(),
            division: String::new(),
            lp: String::new(),
            last_set: default_history(),
        }
    }
}

impl TftRank {
    pub fn error() -> Self {
        Self {
            tier: "Error".to_owned(),
            division: String::new(),
            lp: String::new(),
            last_set: "...".to_owned(),
        }
    }
}

fn default_tier() -> String {
    "Unranked".to_owned()
}

fn default_history() -> String {
    "N/A".to_owned()
}

impl Account {
    pub fn key(&self) -> AccountKey {
        AccountKey {
            account_id: self.account_id.clone(),
            region: self.region.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct AccountKey {
    pub account_id: String,
    pub region: String,
}

/// Values returned by the rank provider.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RankInfo {
    /// `None` when a lookup cannot verify the Riot ID (for example a timeout).
    pub riot_id_not_found: Option<bool>,
    pub tier: String,
    pub division: String,
    pub lp: String,
    pub level: String,
    pub ranked_stats: Option<RankedStats>,
    /// None means history could not be read; Some(empty) is a successful empty result.
    pub recent_games: Option<Vec<RecentGame>>,
    pub reached_last_season: String,
    pub finished_last_season: String,
    /// `None` when the TFT profile could not be fetched.
    pub tft: Option<TftRank>,
    pub provenance: Option<RankProvenance>,
    pub tft_provenance: Option<RankProvenance>,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Freshness {
    Fresh,
    Stale,
    #[default]
    Unknown,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RankFetchError {
    ProfileNotFound,
    RateLimited,
    ProviderUnavailable,
    ParsingFailed,
    Forbidden,
    Timeout,
    RenewalFailed,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct RankProvenance {
    pub source: String,
    /// Unix milliseconds; never substitute fetch time for source update time.
    pub fetched_at: Option<u64>,
    pub source_updated_at: Option<u64>,
    pub freshness: Freshness,
    /// Unix milliseconds after which a fresh source turns stale.
    pub fresh_until: Option<u64>,
    pub renewal_attempted: bool,
    pub renewal_succeeded: bool,
    pub error: Option<RankFetchError>,
}

impl RankInfo {
    pub fn unranked() -> Self {
        Self {
            riot_id_not_found: None,
            tier: "Unranked".to_owned(),
            division: String::new(),
            lp: String::new(),
            level: String::new(),
            reached_last_season: "Unranked".to_owned(),
            finished_last_season: "Unranked".to_owned(),
            tft: Some(TftRank::default()),
            ..Self::default()
        }
    }

    pub fn error() -> Self {
        Self {
            riot_id_not_found: None,
            tier: "Error".to_owned(),
            division: String::new(),
            lp: String::new(),
            level: String::new(),
            reached_last_season: "...".to_owned(),
            finished_last_season: "...".to_owned(),
            tft: None,
            ..Self::default()
        }
    }

    pub fn apply_to(&self, account: &mut Account) {
        // Accounts saved before provenance existed have a level from a past fetch.
        let fetched_before = account.rank_provenance.is_some() || !account.level.is_empty();
        let tft_fetched_before = fetched_before || account.tft_provenance.is_some();
        if let Some(not_found) = self.riot_id_not_found {
            account.riot_id_not_found = not_found;
        }
        // A failed request must not replace the last known standing with Unranked.
        if self.tier != "Error" || self.riot_id_not_found == Some(true) {
            account.tier = if self.tier.is_empty() {
                "Unranked".to_owned()
            } else {
                self.tier.clone()
            };
            account.division = self.division.clone();
            account.lp = self.lp.clone();
            account.level = self.level.clone();
            if let Some(stats) = &self.ranked_stats {
                account.ranked_stats = Some(stats.clone());
            }
            if let Some(games) = &self.recent_games {
                account.recent_games = games.clone();
            }
            account.reached_last_season = if self.reached_last_season.is_empty() {
                "N/A".to_owned()
            } else {
                self.reached_last_season.clone()
            };
            account.finished_last_season = if self.finished_last_season.is_empty() {
                "N/A".to_owned()
            } else {
                self.finished_last_season.clone()
            };
        } else if !fetched_before && (account.tier.is_empty() || account.tier == "Unranked") {
            // No successful fetch yet: show Error rather than an invented standing.
            account.tier = "Error".to_owned();
        }
        if let Some(tft) = &self.tft {
            account.tft = tft.clone();
        } else if !tft_fetched_before && account.tft.tier == "Unranked" {
            account.tft = TftRank::error();
        }
        if self.tier == "Error" && self.provenance.is_none() {
            let provenance = account
                .rank_provenance
                .get_or_insert_with(RankProvenance::default);
            provenance.error = Some(RankFetchError::ProviderUnavailable);
            provenance.freshness = Freshness::Stale;
        } else {
            account.rank_provenance = self.provenance.clone();
        }
        if self.tft.is_none() && self.tft_provenance.is_none() {
            let provenance = account
                .tft_provenance
                .get_or_insert_with(RankProvenance::default);
            provenance.error = Some(RankFetchError::ProviderUnavailable);
            provenance.freshness = Freshness::Stale;
        } else {
            account.tft_provenance = self.tft_provenance.clone();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_match_history_does_not_clear_saved_card_data() {
        let saved = Account {
            ranked_stats: Some(RankedStats {
                wins: 47,
                losses: 33,
                win_rate: 59,
            }),
            recent_games: vec![RecentGame {
                champion: "Zeri".into(),
                icon_url: "https://example.test/Zeri.png".into(),
                won: true,
            }],
            ..Default::default()
        };
        let mut account = saved.clone();
        RankInfo {
            tier: "Diamond".into(),
            ..Default::default()
        }
        .apply_to(&mut account);
        assert_eq!(account.ranked_stats, saved.ranked_stats);
        assert_eq!(account.recent_games, saved.recent_games);
    }
}
