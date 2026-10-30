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
    ("ME", "me"),
    ("SEA", "sea"),
    ("TW", "tw"),
    ("VN", "vn"),
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

/// Split one bulk line into `(account, name, password)`.
///
/// `--` separates fields. Two fields are `account--password` — no Riot ID yet,
/// which is what a bulk import usually has — and three are
/// `account--name--password`.
///
/// The separator is the literal `--`, not a *run* of dashes, so a password
/// that starts with one dash survives: `user---abc` is `user` / `-abc`. A
/// password that starts with `--` cannot be written at all, because those two
/// dashes read as a field boundary and leave an empty field. Every empty field
/// is rejected, so such a password fails to import instead of silently losing
/// its dashes. Keep it that way: trimming dashes "to be forgiving" is exactly
/// how a password gets corrupted without anyone noticing.
pub fn parse_account_line(line: &str) -> Option<(&str, &str, &str)> {
    let parts: Vec<&str> = line.split("--").map(str::trim).collect();
    let (account_id, name, password) = match parts[..] {
        // Two fields: no Riot ID column at all.
        [account_id, password] => (account_id, "", password),
        // Three fields: the middle one must actually carry a name. An empty
        // middle field means the line had a stray separator (`user----secret`),
        // and guessing which side lost a character is worse than refusing it.
        [account_id, name, password] if !name.is_empty() => (account_id, name, password),
        _ => return None,
    };
    (!account_id.is_empty() && !password.is_empty()).then_some((account_id, name, password))
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

pub fn app_data_dir() -> std::io::Result<PathBuf> {
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
    Ok(folder)
}

pub fn accounts_file() -> std::io::Result<PathBuf> {
    Ok(app_data_dir()?.join("league_accounts.json"))
}

pub fn settings_file() -> std::io::Result<PathBuf> {
    Ok(app_data_dir()?.join("settings.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bulk_input_reads_two_and_three_field_lines() {
        // Two fields: no Riot ID yet, which is what a bulk import usually has.
        assert_eq!(parse_account_line("user2--password2"), Some(("user2", "", "password2")));
        assert_eq!(
            parse_account_line("  user2 -- password2  "),
            Some(("user2", "", "password2"))
        );
        // Three fields, with the Riot ID.
        assert_eq!(
            parse_account_line("user1--Name1#TAG1--password1"),
            Some(("user1", "Name1#TAG1", "password1"))
        );
        // Six dashes split into four fields: ambiguous, so rejected.
        assert_eq!(parse_account_line("user------secret"), None);
    }

    #[test]
    fn bulk_input_rejects_lines_that_would_lose_characters() {
        for invalid in [
            "",                      // no separator
            "user",                  // no separator
            "--secret",              // no account
            "user--",                // no password
            "user----",              // two separators, still no password
            "user----secret",        // empty middle field
            "user--name--pass--x",   // too many fields
            "user----password",      // password begins with `--`, so empty field
        ] {
            assert_eq!(parse_account_line(invalid), None, "{invalid:?}");
        }
    }

    #[test]
    fn a_password_may_start_with_a_single_dash() {
        // The separator is literally `--`, so one leading dash stays in the
        // password instead of being swallowed with the separator.
        assert_eq!(parse_account_line("user---abc"), Some(("user", "", "-abc")));
        assert_eq!(
            parse_account_line("user--Name#TAG---abc"),
            Some(("user", "Name#TAG", "-abc"))
        );
    }

    #[test]
    fn rank_sort_key_matches_python_order() {
        assert!(rank_sort_key("Challenger", "", "1") < rank_sort_key("Master", "", "999"));
        assert!(rank_sort_key("Gold", "I", "100") < rank_sort_key("Gold", "II", "999"));
        assert!(rank_sort_key("Gold", "II", "100") < rank_sort_key("Gold", "II", "50"));
    }
}
