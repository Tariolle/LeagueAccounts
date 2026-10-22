//! Optional match-history overlay; rank renewal keeps its upstream implementation.
use super::{
    opgg_puuid, parse_opgg_action_games, parse_opgg_action_page, OpggGamesPage, RankFetcher,
};
use crate::logging::Reason;
use crate::models::{RankFetchError, RecentGame};
use regex::Regex;
use scraper::Html;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

const HISTORY_TIMEOUT: Duration = Duration::from_secs(5);
const ACTION_CACHE_MAX_AGE: Duration = Duration::from_secs(15 * 60);
const FAILED_RETRY_DELAY: Duration = Duration::from_secs(60);
const ASSET_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Default)]
pub(super) struct GamesState {
    action: Mutex<Option<CachedGamesAction>>,
}
struct CachedGamesAction {
    build: String,
    script_url: Option<String>,
    result: Result<String, RankFetchError>,
    at: Instant,
}
impl GamesState {
    fn invalidate(&self, action: &str) {
        // Never wait behind another worker's network discovery just to discard
        // an old action. That worker is already replacing the cached reference.
        if let Ok(mut cache) = self.action.try_lock() {
            if cache
                .as_ref()
                .is_some_and(|cached| cached.result.as_deref() == Ok(action))
            {
                *cache = None;
            }
        }
    }
}

fn remaining(deadline: Instant) -> Result<Duration, RankFetchError> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|d| !d.is_zero())
        .ok_or(RankFetchError::Timeout)
}

impl RankFetcher {
    fn discover_get_games_action(
        &self,
        body: &str,
        deadline: Instant,
    ) -> Result<String, RankFetchError> {
        self.discover_get_games_action_with(body, deadline, |url, timeout| {
            self.request_text(self.client.get(url).timeout(timeout))
                .map_err(super::fetch_error)
        })
    }

    fn discover_get_games_action_with(
        &self,
        body: &str,
        deadline: Instant,
        mut fetch_bundle: impl FnMut(&str, Duration) -> Result<String, RankFetchError>,
    ) -> Result<String, RankFetchError> {
        static SCRIPTS: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(
                r#"<script[^>]+src="(https://c-lol-web\.op\.gg/[^"<>]+/_next/static/chunks/[^"<>]+\.js)""#,
            )
            .expect("valid OP.GG scripts regex")
        });
        static REFERENCE: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(r#"createServerReference\)\("([a-f0-9]{40,})",.{0,160}?"getGames"\)"#)
                .expect("valid getGames action reference regex")
        });
        let scripts: Vec<String> = SCRIPTS
            .captures_iter(body)
            .map(|capture| capture[1].to_owned())
            .collect();
        if scripts.is_empty() || scripts.len() > 64 {
            return Err(RankFetchError::ParsingFailed);
        }
        let build = scripts[0]
            .split("/_next/static/")
            .next()
            .unwrap_or_default()
            .to_owned();
        let mut cache = loop {
            match self.games.action.try_lock() {
                Ok(cache) => break cache,
                Err(std::sync::TryLockError::WouldBlock) => {
                    std::thread::sleep(remaining(deadline)?.min(Duration::from_millis(10)));
                }
                Err(std::sync::TryLockError::Poisoned(_)) => {
                    return Err(RankFetchError::ProviderUnavailable);
                }
            }
        };
        // Time waiting for another worker's discovery is part of this optional
        // lookup's budget, including when that worker has filled the cache.
        remaining(deadline)?;
        if let Some(cached) = &*cache {
            let reusable = if cached.result.is_ok() {
                cached.at.elapsed() < ACTION_CACHE_MAX_AGE
                    && cached
                        .script_url
                        .as_ref()
                        .is_some_and(|url| scripts.contains(url))
            } else {
                cached.at.elapsed() < FAILED_RETRY_DELAY
            };
            if cached.build == build && reusable {
                return cached.result.clone();
            }
        }
        let mut script_url = None;
        let result = (|| {
            for url in scripts.iter().rev() {
                let timeout = remaining(deadline)?.min(ASSET_TIMEOUT);
                let bundle = fetch_bundle(url, timeout)?;
                if let Some(capture) = REFERENCE.captures(&bundle) {
                    script_url = Some(url.clone());
                    return Ok(capture[1].to_owned());
                }
            }
            Err(RankFetchError::ParsingFailed)
        })();
        *cache = Some(CachedGamesAction {
            build,
            script_url,
            result: result.clone(),
            at: Instant::now(),
        });
        result
    }

    pub(super) fn invalidate_get_games_action(&self, action: &str) {
        self.games.invalidate(action);
    }
}

impl RankFetcher {
    pub(super) fn fetch_recent_games_from_opgg(
        &self,
        body: &str,
        url: &str,
        region: &str,
    ) -> Result<Vec<RecentGame>, Reason> {
        let deadline = Instant::now() + HISTORY_TIMEOUT;
        let soup = Html::parse_document(body);
        let puuid = opgg_puuid(&soup).ok_or(Reason::ProfileMissing)?;
        let mut payload = serde_json::json!([{
            "locale": "en", "region": region, "puuid": puuid,
            "gameType": "TOTAL", "endedAt": "", "champion": serde_json::Value::Null,
        }]);
        let mut page = self.request_opgg_games(body, url, &payload, deadline)?;
        if page.data.is_empty() {
            if let Some(cursor) = page
                .meta
                .get("last_game_created_at")
                .and_then(serde_json::Value::as_str)
            {
                payload[0]["endedAt"] = cursor.into();
                page = self.request_opgg_games(body, url, &payload, deadline)?;
            }
        }
        let games = parse_opgg_action_games(&page.data);
        // A response whose rows cannot be understood is not an empty history.
        // Preserve saved games instead of erasing them after a schema change.
        if !page.data.is_empty() && games.is_empty() {
            return Err(Reason::ResponseBody);
        }
        Ok(games)
    }

    fn request_opgg_games(
        &self,
        body: &str,
        url: &str,
        payload: &serde_json::Value,
        deadline: Instant,
    ) -> Result<OpggGamesPage, Reason> {
        let action = self
            .discover_get_games_action(body, deadline)
            .map_err(|_| Reason::ResponseBody)?;
        let request = self
            .client
            .post(url)
            .header("Next-Action", &action)
            .header(reqwest::header::ACCEPT, "text/x-component")
            .header(reqwest::header::CONTENT_TYPE, "text/plain;charset=UTF-8")
            .header(reqwest::header::ORIGIN, "https://op.gg")
            .header(reqwest::header::REFERER, url)
            .body(payload.to_string())
            .timeout(remaining(deadline).map_err(|_| Reason::Timeout)?);
        let response = self.request_text(request).inspect_err(|reason| {
            if *reason == Reason::HttpNotFound {
                self.invalidate_get_games_action(&action);
            }
        })?;
        match parse_opgg_action_page(&response) {
            Some(page) => Ok(page),
            None => {
                self.invalidate_get_games_action(&action);
                Err(Reason::ResponseBody)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{Account, RankInfo, RecentGame};
    use crate::rank_fetcher::RefreshMode;
    use std::io::{BufRead, Read, Write};
    use std::sync::Arc;

    const SCRIPT: &str = "https://c-lol-web.op.gg/test-build/_next/static/chunks/test.js";

    fn profile(short_history: bool) -> String {
        let items = if short_history {
            r#"{"item":{"additionalProperty":[{"name":"champion","value":"Zeri"},{"name":"result","value":"WIN"}]}}"#
        } else {
            ""
        };
        format!(
            r##"<meta name="description" content="Fixture / Diamond II 10 LP / 47Win 33Lose Win rate 59%"><img src="profile_icons/profileIcon6.jpg"><img src="meta/images/lol/16.20.1/champion/Zeri.png"><script src="{SCRIPT}"></script><script type="application/ld+json">{{"@graph":[{{"@type":"Person","identifier":[{{"name":"puuid","value":"fixture-puuid"}}]}},{{"@type":"ItemList","@id":"#recent-games","itemListElement":[{items}]}}]}}</script>"##
        )
    }

    fn cached_fetcher() -> RankFetcher {
        let fetcher = RankFetcher::new();
        *fetcher.games.action.lock().unwrap() = Some(CachedGamesAction {
            build: "https://c-lol-web.op.gg/test-build".into(),
            script_url: Some(SCRIPT.into()),
            result: Ok("games".into()),
            at: Instant::now(),
        });
        fetcher
    }

    type Requests = Arc<Mutex<Vec<serde_json::Value>>>;
    fn server(
        replies: Vec<(&'static str, u16, String)>,
        delay: Duration,
    ) -> (String, Requests, std::thread::JoinHandle<()>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let requests: Requests = Arc::default();
        let received = requests.clone();
        let thread = std::thread::spawn(move || {
            for (method, status, body) in replies {
                let deadline = Instant::now() + Duration::from_secs(4);
                let (mut stream, _) = loop {
                    match listener.accept() {
                        Ok(stream) => break stream,
                        Err(error)
                            if error.kind() == std::io::ErrorKind::WouldBlock
                                && Instant::now() < deadline =>
                        {
                            std::thread::sleep(Duration::from_millis(5))
                        }
                        Err(error) => panic!("Expected {method}: {error}"),
                    }
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut reader = std::io::BufReader::new(&mut stream);
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                assert!(line.starts_with(method), "{line}");
                let mut length = 0;
                let mut action = None;
                loop {
                    line.clear();
                    assert!(reader.read_line(&mut line).unwrap() > 0);
                    if line == "\r\n" {
                        break;
                    }
                    let lower = line.to_ascii_lowercase();
                    if let Some(value) = lower.strip_prefix("content-length:") {
                        length = value.trim().parse().unwrap();
                    }
                    if let Some(value) = lower.strip_prefix("next-action:") {
                        action = Some(value.trim().to_owned());
                    }
                }
                let mut payload = vec![0; length];
                reader.read_exact(&mut payload).unwrap();
                if method == "POST" {
                    assert_eq!(action.as_deref(), Some("games"));
                    let args: serde_json::Value = serde_json::from_slice(&payload).unwrap();
                    assert_eq!(args[0]["puuid"], "fixture-puuid");
                    assert_eq!(args[0]["region"], "kr");
                    assert_eq!(args[0]["gameType"], "TOTAL");
                    received.lock().unwrap().push(args);
                }
                std::thread::sleep(delay);
                let _ = write!(stream, "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nRetry-After: 60\r\nConnection: close\r\n\r\n{body}", body.len());
            }
        });
        (format!("http://{address}"), requests, thread)
    }

    fn page(count: usize) -> String {
        let data: Vec<_> = (0..count).map(|_| serde_json::json!({"game_result":"WIN","champion":{"name":"Zeri","image_url":"https://opgg-static.akamaized.net/meta/images/lol/16.20.1/champion/Zeri.png"}})).collect();
        format!("1:{}", serde_json::json!({"data":data,"meta":{}}))
    }

    #[test]
    fn discovers_and_caches_the_current_match_history_action() {
        let fetcher = RankFetcher::new();
        let action = fetcher
            .discover_get_games_action_with(
                &profile(false),
                Instant::now() + HISTORY_TIMEOUT,
                |_, _| Ok(include_str!("../../tests/fixtures/opgg-actions.js").into()),
            )
            .unwrap();
        assert_eq!(action, "409a2b9ca50d15e50a4dace93552e3a40113dc2753");
        assert_eq!(
            fetcher
                .discover_get_games_action_with(
                    &profile(false),
                    Instant::now() + HISTORY_TIMEOUT,
                    |_, _| panic!("cached match-history action should be reused")
                )
                .unwrap(),
            action
        );
    }

    #[test]
    fn match_history_action_404_invalidates_the_cached_reference() {
        for (status, body) in [
            (404, "missing action"),
            (200, "1:{\"error\":\"old action\"}"),
        ] {
            let fetcher = cached_fetcher();
            let (url, _, server) = server(vec![("POST", status, body.into())], Duration::ZERO);
            assert!(fetcher
                .request_opgg_games(
                    &profile(false),
                    &url,
                    &serde_json::json!([{ "puuid": "fixture-puuid", "region": "kr", "gameType": "TOTAL" }]),
                    Instant::now() + HISTORY_TIMEOUT
                )
                .is_err());
            server.join().unwrap();
            assert!(fetcher.games.action.lock().unwrap().is_none());
            assert!(fetcher
                .discover_get_games_action_with(
                    &profile(false),
                    Instant::now() + HISTORY_TIMEOUT,
                    |_, _| Ok(include_str!("../../tests/fixtures/opgg-actions.js").into())
                )
                .is_ok());
        }
    }

    #[test]
    fn cached_history_action_respects_an_expired_request_budget() {
        let fetcher = cached_fetcher();
        assert_eq!(
            fetcher.discover_get_games_action_with(
                &profile(false),
                Instant::now() - Duration::from_millis(1),
                |_, _| panic!("expired history must not request assets")
            ),
            Err(RankFetchError::Timeout)
        );
    }

    #[test]
    fn history_discovery_wait_is_bounded_while_another_worker_holds_the_cache() {
        let fetcher = cached_fetcher();
        let _held = fetcher.games.action.lock().unwrap();
        fetcher.games.invalidate("games");
        assert_eq!(
            fetcher.discover_get_games_action_with(
                &profile(false),
                Instant::now() + Duration::from_millis(25),
                |_, _| panic!("must not fetch without the cache permit")
            ),
            Err(RankFetchError::Timeout)
        );
    }

    #[test]
    fn history_cache_tracks_asset_rotation_expiry_failure_backoff_and_late_errors() {
        let fetcher = cached_fetcher();
        let rotated = profile(false).replace("test.js", "rotated.js");
        let reference = fetcher
            .discover_get_games_action_with(&rotated, Instant::now() + HISTORY_TIMEOUT, |url, _| {
                assert!(url.ends_with("rotated.js"));
                Ok(include_str!("../../tests/fixtures/opgg-actions.js").into())
            })
            .unwrap();
        fetcher.games.invalidate("games");
        assert_eq!(
            fetcher
                .games
                .action
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .result
                .as_ref()
                .unwrap(),
            &reference
        );
        fetcher.games.action.lock().unwrap().as_mut().unwrap().at =
            Instant::now() - ACTION_CACHE_MAX_AGE;
        assert!(fetcher
            .discover_get_games_action_with(&rotated, Instant::now() + HISTORY_TIMEOUT, |_, _| Ok(
                "changed contract".into()
            ))
            .is_err());
        assert_eq!(
            fetcher.discover_get_games_action_with(
                &rotated,
                Instant::now() + HISTORY_TIMEOUT,
                |_, _| panic!("failed discoveries must back off")
            ),
            Err(RankFetchError::ParsingFailed)
        );
        fetcher.games.action.lock().unwrap().as_mut().unwrap().at =
            Instant::now() - FAILED_RETRY_DELAY;
        assert!(fetcher
            .discover_get_games_action_with(&rotated, Instant::now() + HISTORY_TIMEOUT, |_, _| Ok(
                include_str!("../../tests/fixtures/opgg-actions.js").into()
            ))
            .is_ok());
    }

    #[test]
    fn history_post_cannot_outlive_its_shared_budget() {
        let fetcher = cached_fetcher();
        let (url, _, server) = server(vec![("POST", 200, page(10))], Duration::from_millis(200));
        let started = Instant::now();
        assert!(matches!(fetcher
            .request_opgg_games(
                &profile(false),
                &url,
                &serde_json::json!([{ "puuid": "fixture-puuid", "region": "kr", "gameType": "TOTAL" }]),
                started + Duration::from_millis(75)
            )
            , Err(Reason::Timeout)));
        server.join().unwrap();
    }

    #[test]
    fn optional_history_failures_keep_ranks_tft_and_saved_games() {
        for (status, body) in [
            (403, "forbidden"),
            (404, "missing action"),
            (429, "rate limited"),
            (503, "unavailable"),
            (200, "malformed"),
            (200, "1:{\"data\":[{\"new_schema\":true}],\"meta\":{}}"),
        ] {
            let fetcher = cached_fetcher();
            let (url, _, server) = server(
                vec![
                    ("GET", 200, profile(false)),
                    ("POST", status, body.into()),
                    ("GET", 200, "profile_icons/profileIcon6.jpg".into()),
                ],
                Duration::ZERO,
            );
            let info =
                fetcher.fetch_profiles(&url, &format!("{url}/tft"), "kr", RefreshMode::Manual);
            server.join().unwrap();
            assert_eq!(info.tier, "Diamond");
            assert_eq!(info.tft.as_ref().unwrap().tier, "Unranked");
            assert_eq!(info.provenance.as_ref().unwrap().error, None);
            assert_eq!(info.recent_games, None);
            let saved = vec![RecentGame {
                champion: "Varus".into(),
                icon_url: "saved.png".into(),
                won: false,
            }];
            let mut account = Account {
                recent_games: saved.clone(),
                ..Default::default()
            };
            info.apply_to(&mut account);
            assert_eq!(account.recent_games, saved);
            assert_eq!(account.ranked_stats.unwrap().wins, 47);
        }
    }

    #[test]
    fn short_inline_history_is_completed_and_failed_fallback_keeps_it() {
        for (status, response, expected) in [(200, page(20), 10), (503, "unavailable".into(), 1)] {
            let fetcher = cached_fetcher();
            let (url, _, server) = server(
                vec![("GET", 200, profile(true)), ("POST", status, response)],
                Duration::ZERO,
            );
            let info = fetcher
                .fetch_from_opgg(&url, "kr", RefreshMode::Manual)
                .unwrap();
            server.join().unwrap();
            assert_eq!(info.recent_games.unwrap().len(), expected);
        }
    }

    #[test]
    fn inactive_history_uses_its_cursor_and_successful_empty_history_clears_cache() {
        let fetcher = cached_fetcher();
        let (url, requests, server) = server(vec![("POST", 200, "1:{\"data\":[],\"meta\":{\"last_game_created_at\":\"2026-09-16T16:56:07+09:00\"}}".into()), ("POST", 200, page(20))], Duration::ZERO);
        assert_eq!(
            fetcher
                .fetch_recent_games_from_opgg(&profile(false), &url, "kr")
                .unwrap()
                .len(),
            10
        );
        server.join().unwrap();
        assert_eq!(
            requests.lock().unwrap()[1][0]["endedAt"],
            "2026-09-16T16:56:07+09:00"
        );
        let (url, _, server) = self::server(vec![("POST", 200, page(0))], Duration::ZERO);
        let games = fetcher
            .fetch_recent_games_from_opgg(&profile(false), &url, "kr")
            .unwrap();
        server.join().unwrap();
        let mut account = Account {
            recent_games: vec![RecentGame {
                champion: "Varus".into(),
                icon_url: "saved.png".into(),
                won: false,
            }],
            ..Default::default()
        };
        RankInfo {
            recent_games: Some(games),
            ..Default::default()
        }
        .apply_to(&mut account);
        assert!(account.recent_games.is_empty());
    }
}
