use std::path::PathBuf;

pub const REGION_MAP: &[(&str, &str)] = &[
    ("EUW", "euw"),
    ("EUNE", "eune"),
    ("NA", "na"),
    ("KR", "kr"),
    ("BR", "br"),
    ("JP", "jp"),
    ("OCE", "oce"),
    ("RU", "ru"),
    ("TR", "tr"),
    ("LAN", "lan"),
    ("LAS", "las"),
];

pub const TIER_ORDER: &[&str] = &[
    "Challenger",
    "Grandmaster",
    "Master",
    "Diamond",
    "Emerald",
    "Platinum",
    "Gold",
    "Silver",
    "Bronze",
    "Iron",
    "Unranked",
    "Error",
];

pub fn region_from_display(display: &str) -> Option<&'static str> {
    REGION_MAP
        .iter()
        .find(|(label, _)| *label == display)
        .map(|(_, value)| *value)
}

pub fn region_display(region: &str) -> String {
    REGION_MAP
        .iter()
        .find(|(_, value)| *value == region)
        .map(|(label, _)| (*label).to_owned())
        .unwrap_or_else(|| region.to_owned())
}

/// The same ordering used by the original application: highest tier first,
/// then division I through IV, then descending LP.
pub fn rank_sort_key(tier: &str, division: &str, lp: &str) -> (usize, usize, i32) {
    let tier_index = TIER_ORDER
        .iter()
        .position(|candidate| candidate.eq_ignore_ascii_case(tier))
        .unwrap_or(TIER_ORDER.len());
    let division_upper = division.trim().to_ascii_uppercase();
    let division_index = match division_upper.as_str() {
        "I" | "1" => 0,
        "II" | "2" => 1,
        "III" | "3" => 2,
        "IV" | "4" => 3,
        _ => 9,
    };
    let lp_value = lp.trim().parse::<i32>().unwrap_or(0);
    (tier_index, division_index, -lp_value)
}

pub fn sort_accounts(accounts: &mut [crate::models::Account]) {
    accounts.sort_by_key(|account| rank_sort_key(&account.tier, &account.division, &account.lp));
}

pub fn accounts_file() -> std::io::Result<PathBuf> {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "home directory is unavailable",
            )
        })?;
    let folder = base.join("LeagueAccounts");
    std::fs::create_dir_all(&folder)?;
    Ok(folder.join("league_accounts.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rank_sort_key_matches_python_order() {
        assert!(rank_sort_key("Challenger", "", "1") < rank_sort_key("Master", "", "999"));
        assert!(rank_sort_key("Gold", "I", "100") < rank_sort_key("Gold", "II", "999"));
        assert!(rank_sort_key("Gold", "II", "100") < rank_sort_key("Gold", "II", "50"));
    }
}
