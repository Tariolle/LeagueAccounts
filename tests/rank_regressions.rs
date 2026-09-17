use leagueaccounts::rank_fetcher::RankFetcher;
use scraper::Html;

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
