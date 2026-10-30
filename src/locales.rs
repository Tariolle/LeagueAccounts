//! Game product locales.
//!
//! A product locale is independent of the account's region: Riot lets a TW2
//! account run the game in `zh_MY`, `en_US`, `ja_JP` and so on. The tool keeps
//! the choice per account and applies it through the Riot Client's local API
//! before the game launches, which is exactly what the client's own language
//! picker does.
//!
//! `zh_MY` (Malaysian Chinese) is the only locale Riot ships that renders
//! Simplified Chinese with Traditional Chinese fallbacks for the SEA/TW
//! clusters, so it is also the value used by "汉化" tools.

/// Locales Riot's product registry reports for `league_of_legends` on the
/// `live` patchline, in the order the registry returns them.
pub const GAME_LOCALES: &[&str] = &[
    "ar_AE", "cs_CZ", "de_DE", "el_GR", "en_AU", "en_GB", "en_PH", "en_SG", "en_US", "es_AR",
    "es_ES", "es_MX", "fr_FR", "hu_HU", "it_IT", "ja_JP", "ko_KR", "pl_PL", "pt_BR", "ro_RO",
    "ru_RU", "th_TH", "tr_TR", "vi_VN", "zh_CN", "zh_MY", "zh_TW",
];

/// Display names, so the UI does not have to ship its own table.
pub fn locale_name(locale: &str) -> &'static str {
    match locale {
        "ar_AE" => "العربية",
        "cs_CZ" => "Čeština",
        "de_DE" => "Deutsch",
        "el_GR" => "Ελληνικά",
        "en_AU" => "English (Australia)",
        "en_GB" => "English (United Kingdom)",
        "en_PH" => "English (Philippines)",
        "en_SG" => "English (Singapore)",
        "en_US" => "English (United States)",
        "es_AR" => "Español (Argentina)",
        "es_ES" => "Español (España)",
        "es_MX" => "Español (Latinoamérica)",
        "fr_FR" => "Français",
        "hu_HU" => "Magyar",
        "it_IT" => "Italiano",
        "ja_JP" => "日本語",
        "ko_KR" => "한국어",
        "pl_PL" => "Polski",
        "pt_BR" => "Português (Brasil)",
        "ro_RO" => "Română",
        "ru_RU" => "Русский",
        "th_TH" => "ไทย",
        "tr_TR" => "Türkçe",
        "vi_VN" => "Tiếng Việt",
        // Named by script first: `zh_CN` and `zh_MY` are both Simplified, so
        // "Simplified/Traditional" is what tells them apart in the picker.
        "zh_CN" => "简体中文（中国大陆）",
        "zh_MY" => "简体中文（马来西亚）",
        "zh_TW" => "繁體中文（中國台灣）",
        _ => "English (United States)",
    }
}

/// `zh_CN` needs the product settings file, and its home page is incomplete.
///
/// Two separate facts, both measured on the live client:
///
/// - **Launching.** The registry's `available_locales` lists `zh_MY` and
///   `zh_TW` but not `zh_CN`, and the local API accepts a `zh_CN` write and
///   reads it back without it taking effect. The value the game starts with
///   comes from `settings.locale` in
///   `<product>.live.product_settings.yaml`; writing that file works, which is
///   also how LOLauncher does it.
/// - **Home page.** Riot never published every activity-center page for
///   `zh_CN`, so `rcp-be-lol-activity-center` gets a 404 for pages such as
///   `jade-home` and the home page fails to render. That is a separate problem
///   from launching and is not fixed here.
pub fn needs_homepage_patch(locale: &str) -> bool {
    locale == "zh_CN"
}

/// Locales whose home page renders without extra help.
pub fn home_page_works(locale: &str) -> bool {
    !needs_homepage_patch(locale)
}

/// Whether the game can be started in this locale at all.
///
/// Every locale in [`GAME_LOCALES`] can, `zh_CN` included: the settings file
/// carries the game's `--locale=` argument and the client ships
/// `Global.zh_CN.wad.client`. [`needs_homepage_patch`] describes the home page,
/// which is a different concern.
pub fn is_launchable(locale: &str) -> bool {
    is_known(locale)
}

/// Accept only a locale the client can actually install, so a typo in a config
/// file cannot make the Riot Client reject the launch.
pub fn is_known(locale: &str) -> bool {
    GAME_LOCALES.contains(&locale)
}

/// `zh-my` / `ZH_MY` / ` zh_my ` all normalize to `zh_MY`.
pub fn normalize(value: &str) -> Option<&'static str> {
    let trimmed = value.trim().replace('-', "_");
    GAME_LOCALES
        .iter()
        .find(|locale| locale.eq_ignore_ascii_case(&trimmed))
        .copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_accepts_case_and_dashes() {
        assert_eq!(normalize("zh-my"), Some("zh_MY"));
        assert_eq!(normalize(" ZH_MY "), Some("zh_MY"));
        assert_eq!(normalize("en_us"), Some("en_US"));
        assert_eq!(normalize("xx_YY"), None);
    }

    #[test]
    fn every_locale_has_a_name() {
        for locale in GAME_LOCALES {
            assert!(!locale_name(locale).is_empty(), "{locale}");
            assert!(is_known(locale));
        }
        // Only the locale that *is* the fallback may carry its name.
        assert_eq!(locale_name("en_US"), "English (United States)");
        assert_ne!(locale_name("en_GB"), locale_name("en_US"));
        assert_ne!(locale_name("zh_MY"), locale_name("zh_TW"));
    }

    #[test]
    fn only_zh_cn_needs_the_homepage_patch() {
        for locale in GAME_LOCALES {
            assert_eq!(needs_homepage_patch(locale), *locale == "zh_CN", "{locale}");
            assert_eq!(home_page_works(locale), !needs_homepage_patch(locale), "{locale}");
        }
        assert!(needs_homepage_patch("zh_CN"));
        assert!(!home_page_works("zh_CN"));
        assert!(home_page_works("zh_TW"));
        assert!(home_page_works("zh_MY"));
        // Launching is a separate concern from the home page: the settings
        // file carries the game's --locale= argument, so zh_CN starts too.
        for locale in GAME_LOCALES {
            assert!(is_launchable(locale), "{locale}");
        }
        assert!(is_launchable("zh_CN"));
    }
}
