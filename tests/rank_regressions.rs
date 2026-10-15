use leagueaccounts::rank_fetcher::RankFetcher;
use scraper::Html;

#[test]
fn captured_public_profiles_parse_current_rank_history_and_level_offline() {
    let fetcher = RankFetcher::default();
    let ranked = fetcher
        .parse_profile(include_str!("fixtures/opgg-ranked.html"))
        .unwrap();
    assert_eq!(ranked.tier, "Challenger");
    assert_eq!(ranked.lp, "2210");
    assert_eq!(ranked.level, "945");
    assert_eq!(ranked.reached_last_season, "Challenger 1255LP");
    assert_eq!(ranked.finished_last_season, "Master 285LP");
    let unranked = fetcher
        .parse_profile(include_str!("fixtures/opgg-unranked.html"))
        .unwrap();
    assert_eq!(unranked.tier, "Unranked");
    assert_eq!(unranked.level, "83");
    assert_eq!(
        fetcher.parse_tft_from_opgg("{\"entry\":{truncated").tier,
        "Error"
    );
}

#[test]
fn failed_fetch_preserves_last_known_ranks_and_serializes_provenance() {
    use leagueaccounts::models::{
        Account, Freshness, RankFetchError, RankInfo, RankProvenance, TftRank,
    };
    let mut account = Account {
        tier: "Diamond".into(),
        division: "II".into(),
        lp: "57".into(),
        level: "400".into(),
        tft: TftRank {
            tier: "Master".into(),
            lp: "200".into(),
            ..Default::default()
        },
        ..Default::default()
    };
    RankInfo {
        provenance: Some(RankProvenance {
            source: "OP.GG".into(),
            fetched_at: Some(100),
            source_updated_at: Some(90),
            freshness: Freshness::Stale,
            error: Some(RankFetchError::RateLimited),
            ..Default::default()
        }),
        ..RankInfo::error()
    }
    .apply_to(&mut account);
    assert_eq!(
        (&account.tier, &account.lp, &account.level),
        (&"Diamond".into(), &"57".into(), &"400".into())
    );
    assert_eq!(account.tft.tier, "Master");
    assert_eq!(account.tft.lp, "200");
    let restored: Account =
        serde_json::from_str(&serde_json::to_string(&account).unwrap()).unwrap();
    assert_eq!(
        restored.rank_provenance.unwrap().error,
        Some(RankFetchError::RateLimited)
    );
    RankInfo {
        tier: "Unranked".into(),
        riot_id_not_found: Some(false),
        ..RankInfo::unranked()
    }
    .apply_to(&mut account);
    assert_eq!(account.tier, "Unranked");
    assert!(account.lp.is_empty());
}

#[test]
fn explicit_unranked_description_ignores_other_players_and_history_ranks() {
    let body = r#"<meta name="description" content="Gumayusi#KR1 / Lv. 83"><img src="profile_icons/profileIcon6.jpg">{"tier_info":{"lp":99,"tier":"SILVER","label":"SILVER 1"}}"#;
    let info = RankFetcher::new().parse_profile(body).unwrap();
    assert_eq!(info.tier, "Unranked");
    assert!(info.lp.is_empty());
    assert_eq!(info.level, "83");
    assert!(RankFetcher::new()
        .parse_profile("<html>Just a moment...</html>")
        .is_err());
}

#[test]
fn unranked_profile_survives_description_wording_changes() {
    // The profile icon is the guard against challenge pages; an unfamiliar
    // description must not turn a real unranked profile into an error.
    let body = r#"<meta name="description" content="Player#EUW - Level 83"><img src="profile_icons/profileIcon6.jpg">"#;
    let info = RankFetcher::new().parse_profile(body).unwrap();
    assert_eq!(info.tier, "Unranked");
}

#[test]
fn tft_profile_without_history_is_unranked_not_error() {
    let fetcher = RankFetcher::new();
    for payload in ["profile_icons/profileIcon6.jpg", r#"{"entry":null}"#] {
        let tft = fetcher.parse_tft_from_opgg(payload);
        assert_eq!(tft.tier, "Unranked", "{payload}");
        assert_eq!(tft.last_set, "Unranked", "{payload}");
    }
}

#[test]
fn failure_keeps_legacy_unranked_standings_saved_without_provenance() {
    use leagueaccounts::models::{Account, RankInfo, TftRank};
    let mut legacy = Account {
        tier: "Unranked".into(),
        level: "83".into(),
        tft: TftRank::default(),
        ..Default::default()
    };
    RankInfo::error().apply_to(&mut legacy);
    assert_eq!(legacy.tier, "Unranked");
    assert_eq!(legacy.tft.tier, "Unranked");

    let mut never_fetched = Account {
        tier: "Unranked".into(),
        ..Default::default()
    };
    RankInfo::error().apply_to(&mut never_fetched);
    assert_eq!(never_fetched.tier, "Error");
    assert_eq!(never_fetched.tft.tier, "Error");
}

#[test]
fn missing_riot_id_does_not_attempt_a_profile_lookup() {
    let account = leagueaccounts::models::Account {
        name: "   ".into(),
        ..Default::default()
    };
    assert_eq!(
        RankFetcher::default().fetch_rank(&account),
        leagueaccounts::models::RankInfo::unranked()
    );
}

#[test]
fn missing_id_warning_survives_a_network_failure_and_clears_after_success() {
    use leagueaccounts::models::{Account, RankInfo};
    let mut account = Account {
        name: "Old Name#TAG".into(),
        ..Account::default()
    };
    RankInfo {
        riot_id_not_found: Some(true),
        ..RankInfo::error()
    }
    .apply_to(&mut account);
    assert!(account.riot_id_not_found);
    let json = serde_json::to_string(&account).unwrap();
    account = serde_json::from_str(&json).unwrap();
    RankInfo::error().apply_to(&mut account);
    assert!(account.riot_id_not_found);
    RankInfo {
        riot_id_not_found: Some(false),
        ..RankInfo::unranked()
    }
    .apply_to(&mut account);
    assert!(!account.riot_id_not_found);
}

#[test]
fn original_multiline_python_level_fixture_still_parses() {
    let html = r#"
        <html>
          <head>
            <meta name="description" content="Hide on bush#KR1 / Challenger 1 1952LP / 248Win 186Lose Win rate 57%"/>
          </head>
          <body>
            <img src="https://opgg-static.akamaized.net/meta/images/profile_icons/profileIcon6.jpg"/>
            <div><span>909</span></div>
          </body>
        </html>
    "#;
    assert_eq!(
        RankFetcher::new().parse_level_from_opgg(&Html::parse_document(html), html),
        "909"
    );
}

#[test]
fn react_level_fallback_spans_newlines() {
    let payload = "profile_icons/profileIcon6.jpg\n{\"children\":909}";
    assert_eq!(
        RankFetcher::new().parse_level_from_opgg(&Html::parse_document(""), payload),
        "909"
    );
}

#[test]
fn history_accepts_null_finished_lp_without_a_quote() {
    let payload = r#""season":"S2025","rank_entries":{"high_rank_info":{"tier":"platinum 4","lp":"25"},"rank_info":{"tier":"gold 4","lp":null}}"#;
    assert_eq!(
        RankFetcher::new().parse_last_season_from_opgg(payload),
        ("Platinum IV 25LP".into(), "Gold IV".into())
    );
}

#[test]
fn history_accepts_null_lp_for_both_ranks() {
    let payload = r#""season":"S2025","rank_entries":{"high_rank_info":{"tier":"platinum 4","lp":null},"rank_info":{"tier":"gold 4","lp":null}}"#;
    assert_eq!(
        RankFetcher::new().parse_last_season_from_opgg(payload),
        ("Platinum IV".into(), "Gold IV".into())
    );
}

#[test]
fn history_retains_multiline_matching() {
    let payload = "\"season\":\"S2025\",\"rank_entries\":{\"high_rank_info\":{\"tier\":\"platinum 4\",\"lp\":\"25\",\n\"tier_image_url\":\"\"},\"rank_info\":{\"tier\":\"gold 4\",\"lp\":\"10\"}}";
    assert_eq!(
        RankFetcher::new().parse_last_season_from_opgg(payload),
        ("Platinum IV 25LP".into(), "Gold IV 10LP".into())
    );
}

#[test]
fn history_parses_current_opgg_rank_fields() {
    let payload = r#""season":"S2025 ","rank_entries":{"high_rank_info":{"tier":"challenger","value":"CHALLENGER","division":1,"lp":"1,255","tier_image_url":"","tier_mini_image_url":""},"rank_info":{"tier":"master","value":"MASTER","division":1,"lp":"285","tier_image_url":"","tier_mini_image_url":""}}"#;
    assert_eq!(
        RankFetcher::new().parse_last_season_from_opgg(payload),
        ("Challenger 1255LP".into(), "Master 285LP".into())
    );
}

#[test]
fn history_parses_reordered_fields_and_numeric_lp() {
    let payload = r#""season":"S2025","rank_entries": {
        "rank_info": {"lp":0,"division":4,"value":"GOLD","tier":"gold 4"},
        "high_rank_info": {"lp":25,"division":4,"value":"PLATINUM","tier":"platinum 4"}
    }"#;
    assert_eq!(
        RankFetcher::new().parse_last_season_from_opgg(payload),
        ("Platinum IV 25LP".into(), "Gold IV 0LP".into())
    );
}

#[test]
fn tft_parses_current_set_and_previous_set() {
    let payload = r#"{"seasons":[{"set":18,"setName":"TFTSet18"}],"entry":{"RANKED_TFT":{"splitNumber":18,"tftSetCoreName":"TFTSet18","queueType":"RANKED_TFT","tier":"DIAMOND","rank":"II","leaguePoints":57}},"matchStat":{},"previous":[{"setName":"TFTSet17","entry":{"RANKED_TFT":{"splitNumber":17,"tftSetCoreName":"TFTSet17","tier":"PLATINUM","rank":"I","leaguePoints":12}}},{"setName":"TFTSet16","entry":{"RANKED_TFT":{"tier":"GOLD","rank":"IV","leaguePoints":0}}}]}"#;
    let rank = RankFetcher::default().parse_tft_from_opgg(payload);
    assert_eq!(rank.tier, "Diamond");
    assert_eq!(rank.division, "II");
    assert_eq!(rank.lp, "57");
    assert_eq!(rank.last_set, "Platinum I");
}

#[test]
fn tft_unranked_current_set_ignores_other_queues() {
    let payload = r#"{"entry":{"RANKED_TFT_DOUBLE_UP":{"tier":"SILVER","rank":"III","leaguePoints":21}},"previous":[{"setName":"TFTSet17","entry":{}}]}"#;
    let rank = RankFetcher::default().parse_tft_from_opgg(payload);
    assert_eq!(rank.tier, "Unranked");
    assert_eq!(rank.last_set, "Unranked");
}

#[test]
fn tft_url_uses_the_tft_profile_path() {
    let fetcher = RankFetcher::default();
    assert_eq!(
        fetcher.build_opgg_tft_url("euw", "Hide on bush#KR1"),
        "https://op.gg/tft/summoners/euw/Hide%20on%20bush-KR1"
    );
}
