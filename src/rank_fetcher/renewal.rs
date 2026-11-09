//! OP.GG's public Update button currently uses Next.js server actions.
//! Discover references from the page's own build; never pin action hashes.
use super::{RankFetcher, RefreshMode};
use crate::models::{Freshness, RankFetchError};
use regex::Regex;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Condvar, LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Single freshness threshold for Manual and Auto; the UI ages it via `fresh_until`.
pub const MAX_AGE: Duration = Duration::from_secs(15 * 60);
/// Wall-clock budget for one renewal exchange: shared-reference discovery plus
/// the POST/poll exchange. The permit wait is budgeted separately by
/// `PERMIT_WAIT`, so a queued renewal never eats another one's budget.
pub const RENEWAL_TIMEOUT: Duration = Duration::from_secs(20);
/// Waiting for a permit has its own budget so it never eats the renewal's.
const PERMIT_WAIT: Duration = Duration::from_secs(40);
pub const MAX_CONCURRENT_RENEWALS: usize = 2;
const FAILED_RETRY_DELAY: Duration = Duration::from_secs(60);
const ACTION_CACHE_MAX_AGE: Duration = Duration::from_secs(15 * 60);
/// Cap for any rate-limit cooldown, including provider-supplied `Retry-After`
/// values. 15 minutes covers OP.GG's normal bursts without ever suspending
/// renewals for hours on a malformed or absurd header.
const MAX_COOLDOWN: Duration = Duration::from_secs(15 * 60);
const DEFAULT_COOLDOWN: Duration = Duration::from_secs(60);
const ASSET_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Default)]
pub(super) struct RenewalState {
    active: Mutex<usize>,
    available: Condvar,
    actions: Mutex<Option<CachedActions>>,
    attempts: Mutex<HashMap<String, Instant>>,
    cooldown: Mutex<Option<Instant>>,
}

#[derive(Clone, PartialEq, Eq)]
struct CachedActions {
    build: String,
    /// The asset that supplied both references. Other profile chunks can differ.
    script_url: Option<String>,
    result: Result<Actions, RankFetchError>,
    at: Instant,
}

struct Permit(Arc<RenewalState>);
impl Drop for Permit {
    fn drop(&mut self) {
        if let Ok(mut active) = self.0.active.lock() {
            *active = active.saturating_sub(1);
            self.0.available.notify_one();
        }
    }
}

impl RenewalState {
    fn invalidate_action(&self, action: &str) {
        if let Ok(mut cache) = self.actions.lock() {
            // A late response from an old worker must not erase newer references.
            if cache.as_ref().is_some_and(|cached| {
                cached
                    .result
                    .as_ref()
                    .is_ok_and(|actions| action == actions.renewal || action == actions.status)
            }) {
                *cache = None;
            }
        }
    }

    pub(super) fn rate_limited(&self) -> bool {
        self.cooldown
            .lock()
            .ok()
            .and_then(|value| *value)
            .is_some_and(|until| until > Instant::now())
    }

    /// A provider `Retry-After` is honored up to [`MAX_COOLDOWN`], so a
    /// malformed or absurd header can never suspend renewal indefinitely.
    pub(super) fn set_cooldown(&self, seconds: Option<u64>) {
        let wait = cooldown(seconds);
        if let Ok(mut until) = self.cooldown.lock() {
            let next = Instant::now() + wait;
            *until = Some(until.map_or(next, |previous| previous.max(next)));
        }
    }

    /// Atomically reserve this profile's retry slot. Only call this when the
    /// renewal POST is about to be sent: a pre-flight failure is not an attempt.
    fn admit_attempt(&self, url: &str) -> Result<(), RankFetchError> {
        let mut attempts = self
            .attempts
            .lock()
            .map_err(|_| RankFetchError::ProviderUnavailable)?;
        attempts.retain(|_, at| at.elapsed() < FAILED_RETRY_DELAY);
        if attempts.contains_key(url) {
            return Err(RankFetchError::RenewalFailed);
        }
        attempts.insert(url.to_owned(), Instant::now());
        Ok(())
    }

    fn acquire(self: &Arc<Self>, deadline: Instant) -> Result<Permit, RankFetchError> {
        let mut active = self
            .active
            .lock()
            .map_err(|_| RankFetchError::ProviderUnavailable)?;
        while *active >= MAX_CONCURRENT_RENEWALS {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .ok_or(RankFetchError::Timeout)?;
            let (next, result) = self
                .available
                .wait_timeout(active, remaining)
                .map_err(|_| RankFetchError::ProviderUnavailable)?;
            active = next;
            if result.timed_out() {
                return Err(RankFetchError::Timeout);
            }
        }
        *active += 1;
        Ok(Permit(Arc::clone(self)))
    }
}

pub(super) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}

/// Honor a provider wait up to [`MAX_COOLDOWN`]; a missing value uses the
/// burst default. The cap is the single place that bounds every cooldown.
fn cooldown(seconds: Option<u64>) -> Duration {
    let requested = Duration::from_secs(seconds.unwrap_or(DEFAULT_COOLDOWN.as_secs()).max(1));
    requested.min(MAX_COOLDOWN)
}

pub(super) fn retry_after(value: &str) -> Option<u64> {
    value.parse().ok().or_else(|| {
        let date =
            time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc2822)
                .ok()?;
        let at = u64::try_from(date.unix_timestamp()).ok()?;
        Some(at.saturating_sub(now_ms() / 1000).max(1))
    })
}

pub(super) fn timestamp(value: &Value) -> Option<u64> {
    let parsed = time::OffsetDateTime::parse(
        value.as_str()?,
        &time::format_description::well_known::Rfc3339,
    )
    .ok()?;
    u64::try_from(parsed.unix_timestamp_nanos() / 1_000_000).ok()
}

#[derive(Debug)]
pub(super) struct ProfileUpdate {
    pub puuid: String,
    pub updated_at: Option<u64>,
    pub renewable_at: Option<u64>,
}

impl ProfileUpdate {
    pub fn parse(payload: &str) -> Option<Self> {
        let marker = payload.find("\"initRenewableAt\"")?;
        let start = payload[..marker].rfind('{')?;
        let data = serde_json::Deserializer::from_str(&payload[start..])
            .into_iter::<Value>()
            .next()?
            .ok()?;
        Some(Self {
            puuid: data["puuid"].as_str()?.to_owned(),
            updated_at: timestamp(&data["initUpdatedAt"]),
            renewable_at: timestamp(&data["initRenewableAt"]),
        })
    }

    pub fn should_renew(&self, mode: RefreshMode, now: u64) -> bool {
        self.renewable_at.is_none_or(|at| at <= now)
            && (mode == RefreshMode::Force || freshness(self.updated_at, now) != Freshness::Fresh)
    }
}

pub(super) fn freshness(updated: Option<u64>, now: u64) -> Freshness {
    match updated {
        Some(at) if at <= now + 30_000 && now.saturating_sub(at) <= MAX_AGE.as_millis() as u64 => {
            Freshness::Fresh
        }
        Some(_) => Freshness::Stale,
        None => Freshness::Unknown,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Actions {
    renewal: String,
    status: String,
}

impl Actions {
    fn parse(bundle: &str) -> Option<Self> {
        static REFERENCES: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(
            r#"createServerReference\)\("([a-f0-9]{40,})",.{0,160}?"(renewalStatus|renewal)"\)"#
        ).expect("valid action reference regex")
        });
        let mut renewal = None;
        let mut status = None;
        for capture in REFERENCES.captures_iter(bundle) {
            match &capture[2] {
                "renewal" => renewal = Some(capture[1].to_owned()),
                "renewalStatus" => status = Some(capture[1].to_owned()),
                _ => {}
            }
        }
        Some(Self {
            renewal: renewal?,
            status: status?,
        })
    }
}

#[derive(Debug, PartialEq, Eq)]
enum RenewalResponse {
    Pending(Duration),
    Finished(u64),
}

fn parse_response(body: &str) -> Result<RenewalResponse, RankFetchError> {
    // A Flight stream can have other rows before and after the action result.
    for line in body.lines() {
        let Some((_, row)) = line.split_once(':') else {
            continue;
        };
        let Ok(data) = serde_json::from_str::<Value>(row) else {
            continue;
        };
        let Some(status) = data["status"].as_str() else {
            continue;
        };
        return match status {
            "RENEWING" => Ok(RenewalResponse::Pending(Duration::from_millis(
                data["delay"].as_u64().unwrap_or(1000).clamp(500, 3000),
            ))),
            "RENEWAL_FINISH" => timestamp(&data["lastUpdatedAt"])
                .map(RenewalResponse::Finished)
                .ok_or(RankFetchError::ParsingFailed),
            "TOO_MANY_RENEWALS" => Err(RankFetchError::RateLimited),
            _ => Err(RankFetchError::RenewalFailed),
        };
    }
    Err(RankFetchError::ParsingFailed)
}

/// Pre-flight failures mean no renewal request reached OP.GG.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RenewalError {
    NotSent(RankFetchError),
    Failed(RankFetchError),
}

fn remaining(deadline: Instant) -> Result<Duration, RankFetchError> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|d| !d.is_zero())
        .ok_or(RankFetchError::Timeout)
}

impl RankFetcher {
    fn discover_actions(&self, body: &str, deadline: Instant) -> Result<Actions, RankFetchError> {
        self.discover_actions_with(body, deadline, |url, timeout| {
            self.request_text(self.client.get(url).timeout(timeout))
                .map_err(super::fetch_error)
        })
    }

    fn discover_actions_with(
        &self,
        body: &str,
        deadline: Instant,
        mut fetch_bundle: impl FnMut(&str, Duration) -> Result<String, RankFetchError>,
    ) -> Result<Actions, RankFetchError> {
        static SCRIPTS: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(
            r#"<script[^>]+src="(https://c-lol-web\.op\.gg/[^"<>]+/_next/static/chunks/[^"<>]+\.js)""#
        ).expect("valid OP.GG scripts regex")
        });
        let scripts: Vec<String> = SCRIPTS
            .captures_iter(body)
            .map(|c| c[1].to_owned())
            .collect();
        if scripts.is_empty() || scripts.len() > 64 {
            return Err(RankFetchError::ParsingFailed);
        }
        // Share valid references across profiles, but rediscover on action-asset
        // rotation or expiry even if the release prefix stays the same.
        let build = scripts[0]
            .split("/_next/static/")
            .next()
            .unwrap_or_default()
            .to_owned();
        // Snapshot the shared references, then drop the lock: another profile
        // must be able to read or refresh them while this one fetches assets.
        let snapshot = self
            .renewal
            .actions
            .lock()
            .map_err(|_| RankFetchError::ProviderUnavailable)?
            .clone();
        if let Some(cached) = &snapshot {
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
                if let Some(actions) = Actions::parse(&bundle) {
                    script_url = Some(url.clone());
                    return Ok(actions);
                }
            }
            Err(RankFetchError::ParsingFailed)
        })();
        let mut cache = self
            .renewal
            .actions
            .lock()
            .map_err(|_| RankFetchError::ProviderUnavailable)?;
        // Commit only when no other worker replaced the cache meanwhile: a slow
        // round must not overwrite newer references or a fresh invalidation.
        if *cache == snapshot {
            *cache = Some(CachedActions {
                build,
                script_url,
                result: result.clone(),
                at: Instant::now(),
            });
        }
        result
    }

    fn call_update(
        &self,
        url: &str,
        action: &str,
        arguments: Value,
        deadline: Instant,
    ) -> Result<RenewalResponse, RankFetchError> {
        let result = self
            .request_text(
                self.client
                    .post(url)
                    .header("Next-Action", action)
                    .header("Origin", "https://op.gg")
                    .header("Accept", "text/x-component")
                    .header("Content-Type", "text/plain;charset=UTF-8")
                    .body(json!([arguments]).to_string())
                    .timeout(remaining(deadline)?),
            )
            .map_err(super::fetch_error)
            .and_then(|body| parse_response(&body));
        match result {
            Err(RankFetchError::RateLimited) => self.renewal.set_cooldown(None),
            // A missing action or an unrecognized Flight result can indicate
            // rotated references. Refresh discovery on the next allowed attempt;
            // never replay a POST after an ambiguous timeout/server failure.
            Err(RankFetchError::ProfileNotFound) => {
                self.renewal.invalidate_action(action);
                // The profile page itself was just parsed; a 404 here is the
                // action contract, not a missing profile.
                return Err(RankFetchError::RenewalFailed);
            }
            Err(RankFetchError::ParsingFailed) => self.renewal.invalidate_action(action),
            _ => {}
        }
        result
    }

    /// Renew one profile standing.
    ///
    /// Everything inside is bounded by [`RENEWAL_TIMEOUT`], which starts after
    /// the permit wait and the rate-limit check and covers both reference
    /// discovery and the POST/poll exchange. Pre-flight failures (permit, cooldown,
    /// discovery) report [`RenewalError::NotSent`] and do not consume this
    /// profile's retry slot, so a later normal request can still proceed.
    pub(super) fn renew_profile(
        &self,
        url: &str,
        region: &str,
        body: &str,
        profile: &ProfileUpdate,
    ) -> Result<u64, RenewalError> {
        let _permit = self
            .renewal
            .acquire(Instant::now() + PERMIT_WAIT)
            .map_err(RenewalError::NotSent)?;
        if self.renewal.rate_limited() {
            return Err(RenewalError::NotSent(RankFetchError::RateLimited));
        }
        let deadline = Instant::now() + RENEWAL_TIMEOUT;
        let actions = self
            .discover_actions(body, deadline)
            .map_err(RenewalError::NotSent)?;
        // Admit only now that the POST is one step away: this single check+insert
        // keeps concurrent renewals of the same profile from double-posting, and
        // a failure after admission is reported as a real attempt. Nothing is
        // ever replayed.
        self.renewal
            .admit_attempt(url)
            .map_err(RenewalError::NotSent)?;
        self.poll_renewal(url, region, profile, &actions, deadline)
            .map_err(RenewalError::Failed)
    }

    fn poll_renewal(
        &self,
        url: &str,
        region: &str,
        profile: &ProfileUpdate,
        actions: &Actions,
        deadline: Instant,
    ) -> Result<u64, RankFetchError> {
        let mut status = self.call_update(
            url,
            &actions.renewal,
            json!({"region":region,"puuid":profile.puuid,"isPremiumPrimary":false}),
            deadline,
        )?;
        let mut backoff = Duration::from_millis(500);
        loop {
            match status {
                RenewalResponse::Finished(at) => {
                    // FINISH can also mean "still in cooldown". The caller verifies
                    // the newly fetched page against this timestamp and its age.
                    return Ok(at);
                }
                RenewalResponse::Pending(delay) => {
                    let wait = delay.max(backoff);
                    if wait >= remaining(deadline)? {
                        return Err(RankFetchError::Timeout);
                    }
                    std::thread::sleep(wait);
                    status = self.call_update(
                        url,
                        &actions.status,
                        json!({"region":region,"puuid":profile.puuid}),
                        deadline,
                    )?;
                    backoff = (backoff * 2).min(Duration::from_secs(3));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Use cached references from a synthetic build and an HTTP loopback server.
    // No test contacts OP.GG or depends on public profiles remaining unchanged.
    fn server(
        replies: Vec<(&'static str, u16, String)>,
    ) -> (RankFetcher, String, std::thread::JoinHandle<()>) {
        use std::io::{BufRead, Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let thread = std::thread::spawn(move || {
            for (method, status, body) in replies {
                let deadline = Instant::now() + Duration::from_secs(5);
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
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut reader = std::io::BufReader::new(&mut stream);
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                assert!(line.starts_with(method), "{line}");
                let mut length = 0;
                let mut action = None;
                loop {
                    line.clear();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = value.trim().parse().unwrap();
                    }
                    if let Some(value) = line.to_ascii_lowercase().strip_prefix("next-action:") {
                        action = Some(value.trim().to_owned());
                    }
                }
                let mut payload = vec![0; length];
                reader.read_exact(&mut payload).unwrap();
                if method == "POST" {
                    assert!(matches!(action.as_deref(), Some("renew") | Some("status")));
                    let args: Value = serde_json::from_slice(&payload).unwrap();
                    assert_eq!(args[0]["puuid"], "fixture-own-puuid");
                    assert_eq!(args[0]["region"], "kr");
                }
                write!(stream, "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nRetry-After: 60\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            }
        });
        let fetcher = RankFetcher {
            client: reqwest::blocking::Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(2))
                .build()
                .unwrap(),
            renewal: Arc::default(),
        };
        *fetcher.renewal.actions.lock().unwrap() = Some(CachedActions {
            build: "https://c-lol-web.op.gg/test-build".into(),
            script_url: Some(
                "https://c-lol-web.op.gg/test-build/_next/static/chunks/test.js".into(),
            ),
            result: Ok(Actions {
                renewal: "renew".into(),
                status: "status".into(),
            }),
            at: Instant::now(),
        });
        (fetcher, format!("http://{address}"), thread)
    }

    fn profile(updated_at: u64, lp: u32) -> String {
        let date = time::OffsetDateTime::from_unix_timestamp((updated_at / 1000) as i64)
            .unwrap()
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap();
        format!(
            r#"<meta name="description" content="Fixture / Diamond II {lp} LP"><img src="profile_icons/profileIcon6.jpg"><script src="https://c-lol-web.op.gg/test-build/_next/static/chunks/test.js"></script>{{"puuid":"fixture-own-puuid","initRenewableAt":null,"initUpdatedAt":"{date}"}}"#
        )
    }

    fn finished(at: u64) -> String {
        let date = time::OffsetDateTime::from_unix_timestamp((at / 1000) as i64)
            .unwrap()
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap();
        format!(r#"1:{{"status":"RENEWAL_FINISH","lastUpdatedAt":"{date}"}}"#)
    }

    fn rotated_bundle() -> String {
        include_str!("../../tests/fixtures/opgg-actions.js")
            .replace(
                "405a04669583947dc03eb8c7f367adf28c8f714e86",
                &"a".repeat(40),
            )
            .replace(
                "400c02bdfd8c90756a329b312a7455e73880ad43ec",
                &"b".repeat(40),
            )
    }

    #[test]
    fn cache_rediscovers_rotated_action_asset_under_the_same_release_prefix() {
        let fetcher = RankFetcher::default();
        let body = profile(now_ms(), 10);
        let deadline = Instant::now() + RENEWAL_TIMEOUT;
        let original = fetcher
            .discover_actions_with(&body, deadline, |_, _| {
                Ok(include_str!("../../tests/fixtures/opgg-actions.js").into())
            })
            .unwrap();
        // Match/history chunks can differ between accounts without a deployment.
        let other_profile = format!(
            r#"{body}<script src="https://c-lol-web.op.gg/test-build/_next/static/chunks/other-profile.js"></script>"#
        );
        assert_eq!(
            fetcher
                .discover_actions_with(&other_profile, deadline, |_, _| {
                    panic!("unchanged action asset should stay shared across accounts")
                })
                .unwrap(),
            original
        );
        let changed = body.replace("/chunks/test.js", "/chunks/new-actions.js");
        let updated = fetcher
            .discover_actions_with(&changed, deadline, |url, _| {
                assert!(url.ends_with("/chunks/new-actions.js"));
                Ok(rotated_bundle())
            })
            .unwrap();
        assert_ne!(updated, original);
        assert_eq!(updated.renewal, "a".repeat(40));
        assert_eq!(updated.status, "b".repeat(40));
    }

    #[test]
    fn discovery_expires_successes_and_keeps_failure_backoff_bounded() {
        let fetcher = RankFetcher::default();
        let body = profile(now_ms(), 10);
        let deadline = Instant::now() + RENEWAL_TIMEOUT;
        fetcher
            .discover_actions_with(&body, deadline, |_, _| {
                Ok(include_str!("../../tests/fixtures/opgg-actions.js").into())
            })
            .unwrap();
        fetcher.renewal.actions.lock().unwrap().as_mut().unwrap().at =
            Instant::now() - ACTION_CACHE_MAX_AGE;
        let updated = fetcher
            .discover_actions_with(&body, deadline, |_, _| Ok(rotated_bundle()))
            .unwrap();
        assert_eq!(updated.renewal, "a".repeat(40));

        *fetcher.renewal.actions.lock().unwrap() = None;
        assert_eq!(
            fetcher.discover_actions_with(&body, deadline, |_, _| Err(
                RankFetchError::ProviderUnavailable
            )),
            Err(RankFetchError::ProviderUnavailable)
        );
        assert_eq!(
            fetcher.discover_actions_with(&body, deadline, |_, _| panic!(
                "discovery failure should back off"
            )),
            Err(RankFetchError::ProviderUnavailable)
        );
        fetcher.renewal.actions.lock().unwrap().as_mut().unwrap().at =
            Instant::now() - FAILED_RETRY_DELAY;
        assert!(fetcher
            .discover_actions_with(&body, deadline, |_, _| Ok(rotated_bundle()))
            .is_ok());
    }

    #[test]
    fn action_contract_errors_clear_cache_and_next_discovery_recovers() {
        for (http, body, invalidate) in [
            (404, "Failed to find Server Action", true),
            (200, "0:{}\n1:E{\"digest\":\"unknown_action\"}", true),
            (403, "blocked", false),
            (429, "limited", false),
            (503, "unavailable", false),
            (200, "1:{\"status\":\"RENEWAL_FAILED\"}", false),
        ] {
            let (fetcher, url, thread) = server(vec![("POST", http, body.into())]);
            let result = fetcher.call_update(
                &url,
                "renew",
                json!({"region":"kr","puuid":"fixture-own-puuid"}),
                Instant::now() + RENEWAL_TIMEOUT,
            );
            thread.join().unwrap();
            assert!(result.is_err());
            assert_eq!(
                fetcher.renewal.actions.lock().unwrap().is_none(),
                invalidate,
                "HTTP {http}: {body}"
            );
            if invalidate {
                let updated = fetcher
                    .discover_actions_with(
                        &profile(now_ms(), 10),
                        Instant::now() + RENEWAL_TIMEOUT,
                        |_, _| Ok(rotated_bundle()),
                    )
                    .unwrap();
                assert_eq!(updated.renewal, "a".repeat(40));
                assert_eq!(updated.status, "b".repeat(40));
            }
        }
    }

    #[test]
    fn late_old_action_error_does_not_erase_new_cache_and_poll_error_invalidates() {
        let (fetcher, _, thread) = server(vec![]);
        thread.join().unwrap();
        fetcher
            .renewal
            .actions
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .result = Ok(Actions {
            renewal: "new-renew".into(),
            status: "new-status".into(),
        });
        fetcher.renewal.invalidate_action("renew");
        assert!(fetcher.renewal.actions.lock().unwrap().is_some());
        fetcher.renewal.invalidate_action("new-status");
        assert!(fetcher.renewal.actions.lock().unwrap().is_none());
    }

    #[test]
    fn renewal_polls_then_parses_the_new_document() {
        let now = now_ms();
        let (fetcher, url, thread) = server(vec![
            ("GET", 200, profile(now - 3_600_000, 10)),
            (
                "POST",
                200,
                "1:{\"status\":\"RENEWING\",\"delay\":500}".into(),
            ),
            ("POST", 200, finished(now)),
            ("GET", 200, profile(now, 25)),
        ]);
        let info = fetcher.fetch_rank_from_url(&url, "kr", RefreshMode::Manual);
        thread.join().unwrap();
        assert_eq!(info.lp, "25");
        let provenance = info.provenance.unwrap();
        assert!(provenance.renewal_succeeded);
        assert_eq!(provenance.freshness, Freshness::Fresh);
    }

    #[test]
    fn renewal_failure_falls_back_to_cached_standing_with_explicit_error() {
        for (http, body, expected) in [
            (403, "blocked", RankFetchError::Forbidden),
            (429, "limited", RankFetchError::RateLimited),
            (503, "down", RankFetchError::ProviderUnavailable),
            (
                200,
                "1:{\"status\":\"RENEWAL_FAILED\"}",
                RankFetchError::RenewalFailed,
            ),
            (
                200,
                "1:{\"status\":\"TOO_MANY_RENEWALS\"}",
                RankFetchError::RateLimited,
            ),
            (
                200,
                "1:{\"status\":\"RENEWAL_FINISH\"}",
                RankFetchError::ParsingFailed,
            ),
            // A rotated action reference is not a missing profile.
            (
                404,
                "Failed to find Server Action",
                RankFetchError::RenewalFailed,
            ),
        ] {
            let (fetcher, url, thread) = server(vec![
                ("GET", 200, profile(now_ms() - 3_600_000, 10)),
                ("POST", http, body.into()),
            ]);
            let info = fetcher.fetch_rank_from_url(&url, "kr", RefreshMode::Manual);
            thread.join().unwrap();
            assert_eq!(info.lp, "10");
            let provenance = info.provenance.unwrap();
            assert_eq!(provenance.error, Some(expected));
            assert_eq!(provenance.freshness, Freshness::Stale);
            assert!(!provenance.renewal_succeeded);
            if expected == RankFetchError::RateLimited {
                assert!(fetcher.renewal.rate_limited());
            }
        }
    }

    #[test]
    fn renewal_cooldown_never_blocks_profile_reads_and_skips_are_not_attempts() {
        let (fetcher, url, thread) = server(vec![
            ("GET", 200, profile(now_ms() - 3_600_000, 10)),
            ("GET", 200, profile(now_ms(), 25)),
        ]);
        fetcher.renewal.set_cooldown(Some(60));
        let stale = fetcher.fetch_rank_from_url(&url, "kr", RefreshMode::Manual);
        let fresh = fetcher.fetch_rank_from_url(&url, "kr", RefreshMode::Manual);
        thread.join().unwrap();
        assert_eq!((stale.lp.as_str(), fresh.lp.as_str()), ("10", "25"));
        let provenance = stale.provenance.unwrap();
        assert_eq!(provenance.error, Some(RankFetchError::RateLimited));
        assert!(!provenance.renewal_attempted);
        let provenance = fresh.provenance.unwrap();
        assert_eq!(provenance.error, None);
        assert!(provenance.fresh_until.is_some());
        assert!(
            !fetcher.renewal.attempts.lock().unwrap().contains_key(&url),
            "a cooldown skip never reached the POST and is not an attempt"
        );
    }

    #[test]
    fn discovery_failure_is_not_sent_and_does_not_consume_the_retry_slot() {
        let now = now_ms();
        let (fetcher, url, thread) = server(vec![
            (
                "POST",
                200,
                "1:{\"status\":\"RENEWING\",\"delay\":50}".into(),
            ),
            ("POST", 200, finished(now)),
        ]);
        let update = ProfileUpdate::parse(&profile(now - 3_600_000, 10)).unwrap();
        // A page we cannot read a bundle from fails before any POST exists.
        assert_eq!(
            fetcher.renew_profile(
                &url,
                "kr",
                r#"<img src="profile_icons/profileIcon6.jpg">"#,
                &update
            ),
            Err(RenewalError::NotSent(RankFetchError::ParsingFailed))
        );
        assert!(fetcher.renewal.attempts.lock().unwrap().is_empty());
        // The pre-flight failure must not block the next normal renewal.
        assert!(fetcher
            .renew_profile(&url, "kr", &profile(now - 3_600_000, 10), &update)
            .is_ok());
        thread.join().unwrap();
    }

    #[test]
    fn renewal_admission_is_per_profile_and_survives_only_the_retry_window() {
        let state = RenewalState::default();
        state.admit_attempt("https://op.gg/lol/summoners/kr/a").unwrap();
        assert_eq!(
            state.admit_attempt("https://op.gg/lol/summoners/kr/a"),
            Err(RankFetchError::RenewalFailed)
        );
        assert!(state.admit_attempt("https://op.gg/lol/summoners/kr/b").is_ok());
        state
            .attempts
            .lock()
            .unwrap()
            .insert(
                "https://op.gg/lol/summoners/kr/c".into(),
                Instant::now() - FAILED_RETRY_DELAY,
            );
        assert!(state.admit_attempt("https://op.gg/lol/summoners/kr/c").is_ok());
    }

    #[test]
    fn action_discovery_never_holds_the_cache_lock_across_requests() {
        let fetcher = RankFetcher::default();
        let body = profile(now_ms(), 10);
        let result = fetcher
            .discover_actions_with(&body, Instant::now() + RENEWAL_TIMEOUT, |_, _| {
                // A profile read or another discovery must be able to proceed.
                assert!(fetcher.renewal.actions.try_lock().is_ok());
                Ok(rotated_bundle())
            })
            .unwrap();
        assert_eq!(result.renewal, "a".repeat(40));
        assert!(fetcher.renewal.actions.lock().unwrap().is_some());
    }

    #[test]
    fn a_slow_discovery_round_does_not_overwrite_newer_references() {
        let fetcher = RankFetcher::default();
        let body = profile(now_ms(), 10);
        let newer = Actions {
            renewal: "c".repeat(40),
            status: "d".repeat(40),
        };
        let discovered = fetcher
            .discover_actions_with(&body, Instant::now() + RENEWAL_TIMEOUT, |_, _| {
                // A newer round publishes while this one is still fetching.
                *fetcher.renewal.actions.lock().unwrap() = Some(CachedActions {
                    build: "https://c-lol-web.op.gg/newer-build".into(),
                    script_url: None,
                    result: Ok(newer.clone()),
                    at: Instant::now(),
                });
                Ok(rotated_bundle())
            })
            .unwrap();
        // This round still reports what it discovered...
        assert_eq!(discovered.renewal, "a".repeat(40));
        // ...but the newer cache wins instead of being overwritten.
        let cached = fetcher.renewal.actions.lock().unwrap();
        let cached = cached.as_ref().expect("newer cache retained");
        assert_eq!(cached.build, "https://c-lol-web.op.gg/newer-build");
        assert_eq!(cached.result.as_ref(), Ok(&newer));
    }

    #[test]
    fn retry_after_is_honored_but_bounded() {
        assert_eq!(cooldown(None), DEFAULT_COOLDOWN);
        assert_eq!(cooldown(Some(0)), Duration::from_secs(1));
        assert_eq!(cooldown(Some(120)), Duration::from_secs(120));
        assert_eq!(cooldown(Some(u64::MAX)), MAX_COOLDOWN);
        let state = RenewalState::default();
        state.set_cooldown(Some(u64::MAX));
        assert!(state.rate_limited());
        let until = state.cooldown.lock().unwrap().unwrap();
        assert!(until <= Instant::now() + MAX_COOLDOWN);
    }

    #[test]
    fn finish_with_old_html_is_stale_and_recent_html_needs_no_renewal() {
        let old = now_ms() - 3_600_000;
        let (fetcher, url, thread) = server(vec![
            ("GET", 200, profile(old, 10)),
            ("POST", 200, finished(now_ms())),
            ("GET", 200, profile(old, 10)),
        ]);
        let info = fetcher.fetch_rank_from_url(&url, "kr", RefreshMode::Manual);
        thread.join().unwrap();
        assert_eq!(
            info.provenance.unwrap().error,
            Some(RankFetchError::RenewalFailed)
        );
        let (fetcher, url, thread) = server(vec![("GET", 200, profile(now_ms(), 25))]);
        let info = fetcher.fetch_rank_from_url(&url, "kr", RefreshMode::Manual);
        thread.join().unwrap();
        assert!(!info.provenance.unwrap().renewal_attempted);
    }

    #[test]
    fn update_component_uses_own_puuid_not_a_truncated_or_other_players_id() {
        let payload = r#"{"puuid":"another-player"} {"puuid":"full-correct-id","initRenewableAt":"2026-10-14T15:54:49+09:00","initUpdatedAt":"2026-10-14T15:52:49+09:00"}"#;
        let profile = ProfileUpdate::parse(payload).unwrap();
        assert_eq!(profile.puuid, "full-correct-id");
        let now = profile.updated_at.unwrap() + 180_000;
        // Manual and Auto share one threshold; only Force renews a fresh profile.
        assert!(!profile.should_renew(RefreshMode::Manual, now));
        assert!(!profile.should_renew(RefreshMode::Auto, now));
        assert!(profile.should_renew(RefreshMode::Force, now));
        let old = profile.updated_at.unwrap() + MAX_AGE.as_millis() as u64 + 1_000;
        assert!(profile.should_renew(RefreshMode::Manual, old));
        assert!(profile.should_renew(RefreshMode::Auto, old));
        assert!(!profile.should_renew(RefreshMode::Force, now - 120_000));
        assert_eq!(freshness(None, now), Freshness::Unknown);
        assert_eq!(freshness(Some(now + 60_000), now), Freshness::Stale);
    }

    #[test]
    fn renewal_request_timeout_is_categorized_and_does_not_report_success() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut buffer = [0; 4096];
            let _ = stream.read(&mut buffer);
            std::thread::sleep(Duration::from_millis(200));
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}");
        });
        let fetcher = RankFetcher {
            client: reqwest::blocking::Client::builder()
                .no_proxy()
                .build()
                .unwrap(),
            renewal: Arc::default(),
        };
        fetcher
            .discover_actions_with(
                &profile(now_ms(), 10),
                Instant::now() + RENEWAL_TIMEOUT,
                |_, _| Ok(rotated_bundle()),
            )
            .unwrap();
        assert_eq!(
            fetcher.call_update(
                &url,
                &"a".repeat(40),
                json!({}),
                Instant::now() + Duration::from_millis(100)
            ),
            Err(RankFetchError::Timeout)
        );
        // The timed-out POST may have been accepted upstream; keep the valid
        // references and let normal scheduling/cooldowns govern the next attempt.
        assert!(fetcher.renewal.actions.lock().unwrap().is_some());
        server.join().unwrap();
    }

    #[test]
    fn renewal_flight_responses_are_not_http_success_alone() {
        assert_eq!(
            parse_response("0:{}\n1:{\"status\":\"RENEWING\",\"delay\":1000}"),
            Ok(RenewalResponse::Pending(Duration::from_secs(1)))
        );
        assert!(matches!(
            parse_response(include_str!("../../tests/fixtures/opgg-renewal-finish.txt")),
            Ok(RenewalResponse::Finished(_))
        ));
        for status in ["RENEWAL_FAILED", "REQUEST_FAILED", "UNKNOWN"] {
            assert_eq!(
                parse_response(&format!("1:{{\"status\":\"{status}\"}}")),
                Err(RankFetchError::RenewalFailed)
            );
        }
        assert_eq!(
            parse_response("1:{\"status\":\"TOO_MANY_RENEWALS\"}"),
            Err(RankFetchError::RateLimited)
        );
        assert_eq!(
            parse_response("1:{\"status\":\"RENEWAL_FINISH\"}"),
            Err(RankFetchError::ParsingFailed)
        );
        assert_eq!(
            parse_response("<html>Just a moment...</html>"),
            Err(RankFetchError::ParsingFailed)
        );
    }

    #[test]
    fn discovers_both_actions_and_rejects_missing_or_old_contract() {
        let bundle = include_str!("../../tests/fixtures/opgg-actions.js");
        assert!(Actions::parse(bundle).is_some());
        assert!(Actions::parse("renewalStatus: '/old/renewal'").is_none());
        assert_eq!(
            remaining(Instant::now() - Duration::from_secs(1)),
            Err(RankFetchError::Timeout)
        );
    }

    #[test]
    fn permits_bound_concurrency_and_are_released_after_failure() {
        let state = Arc::new(RenewalState::default());
        let deadline = Instant::now() + Duration::from_millis(10);
        let first = state.acquire(deadline).unwrap();
        let second = state.acquire(deadline).unwrap();
        assert!(matches!(
            state.acquire(deadline),
            Err(RankFetchError::Timeout)
        ));
        drop(first);
        assert!(state
            .acquire(Instant::now() + Duration::from_millis(10))
            .is_ok());
        drop(second);
        state.set_cooldown(Some(60));
        assert!(state.rate_limited());
        assert_eq!(retry_after("120"), Some(120));
        assert!(retry_after("Wed, 14 Oct 2026 07:01:00 GMT").is_some());
        assert_eq!(retry_after("invalid"), None);
    }
}
