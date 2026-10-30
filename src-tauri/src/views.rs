//! Data shapes sent to the web interface. Passwords never appear here.

use leagueaccounts::models::{Account, AccountKey, Ban, RankProvenance};
use serde::{Deserialize, Serialize};

/// A command failure the UI translates: a stable `code` plus an optional
/// untranslated detail (for example an OS error message).
#[derive(Clone, Debug, Serialize)]
pub struct AppError {
    pub code: &'static str,
    pub detail: Option<String>,
}

pub fn fail(code: &'static str) -> AppError {
    AppError { code, detail: None }
}

pub fn fail_with(code: &'static str, detail: impl std::fmt::Display) -> AppError {
    AppError {
        code,
        detail: Some(detail.to_string()),
    }
}

pub type CommandResult<T> = Result<T, AppError>;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Key {
    pub account_id: String,
    pub region: String,
}

impl From<Key> for AccountKey {
    fn from(key: Key) -> Self {
        Self {
            account_id: key.account_id,
            region: key.region,
        }
    }
}

impl From<&Account> for Key {
    fn from(account: &Account) -> Self {
        Self {
            account_id: account.account_id.clone(),
            region: account.region.clone(),
        }
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LolView {
    tier: String,
    division: String,
    lp: String,
    reached_last_season: String,
    finished_last_season: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TftView {
    tier: String,
    division: String,
    lp: String,
    last_set: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountView {
    account_id: String,
    name: String,
    riot_id_not_found: bool,
    region: String,
    region_display: String,
    description: String,
    level: String,
    has_password: bool,
    /// Product locale this account starts the game in; `None` = Riot's choice.
    locale: Option<String>,
    skip_profile_update: bool,
    lol: LolView,
    tft: TftView,
    rank_provenance: Option<RankProvenance>,
    tft_provenance: Option<RankProvenance>,
    ban: Option<Ban>,
}

pub fn account_view(account: &Account) -> AccountView {
    AccountView {
        account_id: account.account_id.clone(),
        name: account.name.clone(),
        riot_id_not_found: account.riot_id_not_found,
        region: account.region.clone(),
        region_display: account.region_display.clone(),
        description: account.description.clone(),
        level: account.level.clone(),
        has_password: !account.password.is_empty(),
        locale: account.locale.clone(),
        skip_profile_update: account.skip_profile_update,
        rank_provenance: account.rank_provenance.clone(),
        tft_provenance: account.tft_provenance.clone(),
        ban: account.ban,
        lol: LolView {
            tier: account.tier.clone(),
            division: account.division.clone(),
            lp: account.lp.clone(),
            reached_last_season: account.reached_last_season.clone(),
            finished_last_season: account.finished_last_season.clone(),
        },
        tft: TftView {
            tier: account.tft.tier.clone(),
            division: account.tft.division.clone(),
            lp: account.tft.lp.clone(),
            last_set: account.tft.last_set.clone(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_responses_preserve_language_and_vn_preferences_without_passwords() {
        let account = Account {
            locale: Some("zh_MY".into()),
            skip_profile_update: true,
            password: "dummy-password".into(),
            ..Account::default()
        };
        let json = serde_json::to_value(account_view(&account)).unwrap();
        assert_eq!(json["locale"], "zh_MY");
        assert_eq!(json["skipProfileUpdate"], true);
        assert_eq!(json["hasPassword"], true);
        assert!(json.get("password").is_none());
    }
}
