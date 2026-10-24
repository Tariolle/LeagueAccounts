//! Riot login through the native renderer and local session API.
//! No keyboard input is injected; game launch requires the selected account.

#[path = "riot_renderer.rs"]
mod renderer;

use crate::logging::{self, Event, Reason};
use crate::models::Ban;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Allow cold starts and client updates without relaxing login readiness.
const LOGIN_TIMEOUT: Duration = Duration::from_secs(120);
/// Time the player has for steps Riot asks for before the game, as for
/// confirming the sign-in.
const SESSION_TIMEOUT: Duration = Duration::from_secs(300);
/// Time for the Riot Client to apply a region change.
const REGION_TIMEOUT: Duration = Duration::from_secs(10);
const POLL: Duration = Duration::from_millis(500);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Game {
    League,
    Tft,
}

impl Game {
    fn product(self) -> &'static str {
        match self {
            Game::League => "league_of_legends",
            Game::Tft => "teamfighttactics",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoginOutcome {
    /// The selected account is confirmed signed in.
    SignedIn,
    /// Credentials were typed in explicit previous-window mode.
    Typed,
    /// This account was already signed in; nothing was typed.
    AlreadySignedIn,
}

/// Progress reported to the UI while signing in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoginStep {
    CloseLeague,
    OpenClient,
    WaitAuth,
    SignOut,
    Connect,
    FindWindow,
    Verification,
    Focus,
    Type,
    Confirm,
    LaunchGame,
    Done,
}

impl LoginStep {
    pub fn record(self) {
        let reason = match self {
            Self::CloseLeague => Reason::CloseLeague,
            Self::OpenClient => Reason::OpenClient,
            Self::WaitAuth => Reason::WaitAuth,
            Self::SignOut => Reason::SignOut,
            Self::Connect => Reason::ConnectRenderer,
            Self::Verification => Reason::AuthInteraction,
            Self::FindWindow => Reason::FindWindow,
            Self::Focus => Reason::Focus,
            Self::Type => Reason::Type,
            Self::Confirm => Reason::Confirm,
            Self::LaunchGame => Reason::LaunchGame,
            Self::Done => Reason::Done,
        };
        logging::record(Event::LoginProgress, reason);
    }

    pub fn code(self) -> &'static str {
        match self {
            LoginStep::CloseLeague => "closeLeague",
            LoginStep::OpenClient => "openClient",
            LoginStep::WaitAuth => "waitAuth",
            LoginStep::SignOut => "signOut",
            LoginStep::Connect => "connect",
            LoginStep::Verification => "verification",
            LoginStep::FindWindow => "findWindow",
            LoginStep::Focus => "focus",
            LoginStep::Type => "type",
            LoginStep::Confirm => "confirm",
            LoginStep::LaunchGame => "launchGame",
            LoginStep::Done => "done",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoginError {
    ClientMissing,
    LaunchFailed,
    /// The Riot Client runs, or must run, as administrator, so this app can
    /// neither start nor close it.
    RiotElevated,
    /// The Riot Client did not close before the login form could be opened.
    RiotCloseFailed,
    /// The login window or sign-in state never became observable.
    Timeout,
    /// Signing out of the current session is not available.
    SignOutFailed,
    /// The Riot session signed out before the game could be launched.
    SessionEnded,
    /// The Riot Client did not switch to the account's League region.
    RegionFailed,
    RendererFailed,
    AuthRejected,
    CaptchaRejected,
    RateLimited,
    TypingFailed,
    /// Cancelled from the UI.
    Cancelled,
    /// A League/TFT client or match is open for another account and closing
    /// it was not confirmed.
    LeagueRunning,
    /// The League/TFT client did not close.
    CloseFailed,
    /// The game client did not confirm the selected account in time.
    GameLaunchFailed,
    /// Riot or League reports a different account during launch.
    AccountMismatch,
    /// Riot reports a League suspension; the game would disconnect at once.
    Banned(Ban),
    WorkerPanicked,
}

impl LoginError {
    pub fn code(self) -> &'static str {
        match self {
            LoginError::ClientMissing => "riot_client_missing",
            LoginError::LaunchFailed => "riot_launch_failed",
            LoginError::RiotElevated => "riot_elevated",
            LoginError::RiotCloseFailed => "riot_close_failed",
            LoginError::Timeout => "riot_timeout",
            LoginError::SignOutFailed => "riot_sign_out_failed",
            LoginError::SessionEnded => "riot_session_ended",
            LoginError::RegionFailed => "riot_region_failed",
            LoginError::RendererFailed => "riot_renderer_failed",
            LoginError::AuthRejected => "riot_auth_rejected",
            LoginError::CaptchaRejected => "riot_captcha_rejected",
            LoginError::RateLimited => "riot_rate_limited",
            LoginError::TypingFailed => "autotype_failed",
            LoginError::Cancelled => "login_cancelled",
            LoginError::LeagueRunning => "league_running",
            LoginError::CloseFailed => "league_close_failed",
            LoginError::GameLaunchFailed => "game_launch_failed",
            LoginError::AccountMismatch => "game_account_mismatch",
            LoginError::Banned(_) => "account_banned",
            LoginError::WorkerPanicked => "autotype_failed",
        }
    }

    /// Only fixed categories cross the logging boundary; never account identifiers.
    fn reason(self) -> Reason {
        match self {
            LoginError::ClientMissing => Reason::ClientMissing,
            LoginError::LaunchFailed => Reason::LaunchFailed,
            LoginError::RiotElevated => Reason::RiotElevated,
            LoginError::RiotCloseFailed => Reason::RiotCloseFailed,
            LoginError::Timeout => Reason::Timeout,
            LoginError::SignOutFailed => Reason::SignOutFailed,
            LoginError::SessionEnded => Reason::SessionEnded,
            LoginError::RegionFailed => Reason::RegionFailed,
            LoginError::RendererFailed => Reason::RendererFailed,
            LoginError::AuthRejected => Reason::AuthRejected,
            LoginError::CaptchaRejected => Reason::CaptchaRejected,
            LoginError::RateLimited => Reason::HttpRateLimited,
            LoginError::TypingFailed => Reason::TypingFailed,
            LoginError::Cancelled => Reason::Cancelled,
            LoginError::LeagueRunning => Reason::LeagueRunning,
            LoginError::CloseFailed => Reason::CloseFailed,
            LoginError::GameLaunchFailed => Reason::GameLaunchFailed,
            LoginError::AccountMismatch => Reason::AccountMismatch,
            LoginError::Banned(_) => Reason::AccountBanned,
            LoginError::WorkerPanicked => Reason::WorkerPanicked,
        }
    }
}

pub fn record_login_result(result: &Result<LoginOutcome, LoginError>) {
    let (event, reason) = match result {
        Ok(outcome) => (
            Event::LoginCompleted,
            match outcome {
                LoginOutcome::SignedIn => Reason::SignedIn,
                LoginOutcome::Typed => Reason::Typed,
                LoginOutcome::AlreadySignedIn => Reason::AlreadySignedIn,
            },
        ),
        Err(error) => (
            if *error == LoginError::Cancelled {
                Event::LoginCancelled
            } else {
                Event::LoginFailed
            },
            error.reason(),
        ),
    };
    logging::record(event, reason);
}

fn program_data() -> PathBuf {
    std::env::var_os("ProgramData")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"))
}

/// Path of `RiotClientServices.exe` from Riot's install registry file.
pub fn client_path() -> Option<PathBuf> {
    let installs = program_data().join(r"Riot Games\RiotClientInstalls.json");
    let from_file = std::fs::read_to_string(installs)
        .ok()
        .and_then(|json| serde_json::from_str::<serde_json::Value>(&json).ok())
        .and_then(|value| {
            ["rc_default", "rc_live"]
                .iter()
                .find_map(|key| value[*key].as_str().map(PathBuf::from))
        })
        .filter(|path| path.exists());
    from_file.or_else(|| {
        let fallback = PathBuf::from(r"C:\Riot Games\Riot Client\RiotClientServices.exe");
        fallback.exists().then_some(fallback)
    })
}

/// Whether Riot's metadata lists the product's live patchline as installed.
pub fn game_installed(game: Game) -> bool {
    program_data()
        .join(r"Riot Games\Metadata")
        .join(format!("{}.live", game.product()))
        .exists()
}

/// Start `game` for the signed-in account and wait until its client process
/// confirms the same account. Requests go through the local API, after
/// rechecking the Riot session immediately before each launch request.
fn launch_game(game: Game, account_id: &str, cancel: &AtomicBool) -> Result<(), LoginError> {
    launch_game_with(
        account_id,
        cancel,
        status,
        || {
            let Some(api) = LocalApi::read().filter(|api| api.session() == Session::SignedIn)
            else {
                return Ok(Launch::Rejected);
            };
            let Some(name) = api.username() else {
                return Ok(Launch::Rejected);
            };
            check_cancelled(cancel)?;
            if !name.eq_ignore_ascii_case(account_id) {
                return Err(LoginError::AccountMismatch);
            }
            api.follow_league_region(REGION_TIMEOUT, cancel)?;
            Ok(api.launch(game))
        },
        || thread::sleep(Duration::from_secs(1)),
    )
}

/// Riot answers a launch request only once the game process exists, which
/// can take half a minute.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Launch {
    Accepted,
    /// No answer yet; Riot keeps launching after the request times out and
    /// answers repeated requests with "already launched".
    Pending,
    Rejected,
}

fn launch_game_with(
    account_id: &str,
    cancel: &AtomicBool,
    mut observe: impl FnMut() -> GameStatus,
    mut request: impl FnMut() -> Result<Launch, LoginError>,
    mut wait: impl FnMut(),
) -> Result<(), LoginError> {
    let mut requested = false;
    let mut diagnostics = LoginDiagnostics::default();
    for attempt in 0..40 {
        check_cancelled(cancel)?;
        let current = observe();
        check_cancelled(cancel)?;
        if launch_confirmed(&current, account_id)? {
            return Ok(());
        }
        // Never launch through a restored or newly changed Riot session.
        // An existing game with an unreadable identity must become observable
        // before it can count as success; do not launch another copy over it.
        let mut reason = if current.league_running() {
            Reason::GameIdentityPending
        } else if current.signed_in_as(account_id) {
            Reason::GameProcessPending
        } else {
            Reason::AuthUnavailable
        };
        if !requested
            && !current.league_running()
            && current.signed_in_as(account_id)
            && attempt % 2 == 0
        {
            match request()? {
                Launch::Accepted => requested = true,
                Launch::Pending => reason = Reason::LaunchPending,
                Launch::Rejected => reason = Reason::LaunchRejected,
            }
        }
        diagnostics.waiting(reason);
        wait();
    }
    check_cancelled(cancel)?;
    Err(LoginError::GameLaunchFailed)
}

fn launch_confirmed(current: &GameStatus, account_id: &str) -> Result<bool, LoginError> {
    let different = |name: &Option<String>| {
        name.as_deref()
            .is_some_and(|name| !name.eq_ignore_ascii_case(account_id))
    };
    if different(&current.signed_in_user)
        || (current.league_running() && different(&current.game_user))
    {
        return Err(LoginError::AccountMismatch);
    }
    Ok(current.league_running() && current.signed_in_as(account_id))
}

/// Launch the requested game; TFT falls back to the League client, which
/// also hosts TFT, when the standalone TFT client cannot start.
fn start_game(game: Game, account_id: &str, cancel: &AtomicBool) -> Result<(), LoginError> {
    match launch_game(game, account_id, cancel) {
        Err(LoginError::GameLaunchFailed) if game == Game::Tft => {
            launch_game(Game::League, account_id, cancel)
        }
        result => result,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Session {
    SignedIn,
    SignedOut,
    Unknown(Reason),
}

/// The lifecycle endpoint is available before RSO initializes on a cold start.
/// Only its explicit login-strategy state permits typing; neither HTTP errors
/// nor an absent RSO authorization prove that the login form is ready.
fn session_state(response: Option<(u16, serde_json::Value)>) -> Session {
    let Some((200, body)) = response else {
        return Session::Unknown(Reason::AuthUnavailable);
    };
    if has_identity(&body) {
        return Session::SignedIn;
    }
    match body["loginState"].as_str() {
        Some("PendingLoginStrategy") => Session::SignedOut,
        Some(_) if body["actionRequired"] == true => Session::Unknown(Reason::AuthInteraction),
        Some(_) => Session::Unknown(Reason::AuthStarting),
        None => Session::Unknown(Reason::AuthUnavailable),
    }
}

fn has_identity(body: &serde_json::Value) -> bool {
    body["puuid"].as_str().is_some_and(|id| !id.is_empty())
}

/// The identity exists before Riot finishes the session: new terms, a League
/// region or a Riot ID can still need the player. Only a session waiting for
/// a product can be handed to a game; one launched earlier cannot log in.
/// The session stays in that state while and after a game runs.
fn product_context(response: Option<(u16, serde_json::Value)>) -> Result<(), Reason> {
    let Some((200, body)) = response else {
        return Err(Reason::AuthUnavailable);
    };
    let state = body["loginState"].as_str();
    // Signed out meanwhile, for example from the Riot window. The login form
    // also requires an action, so check this first.
    if state == Some("PendingLoginStrategy") && !has_identity(&body) {
        return Err(Reason::SessionEnded);
    }
    if body["actionRequired"] == true {
        return Err(Reason::SessionActionRequired);
    }
    match state {
        Some("PendingProductContext") => Ok(()),
        _ => Err(Reason::SessionPending),
    }
}

/// Wait until the signed-in session can be handed to a game, showing the
/// Riot window once if Riot needs the player first.
fn await_product_context(
    client: &mut renderer::Renderer,
    progress: &mut dyn FnMut(LoginStep),
    cancel: &AtomicBool,
) -> Result<(), LoginError> {
    await_product_context_with(
        cancel,
        SESSION_TIMEOUT,
        || {
            LocalApi::read().map_or(Err(Reason::AuthUnavailable), |api| {
                product_context(api.get("/player-session-lifecycle/v1/session"))
            })
        },
        || {
            client.show()?;
            progress(LoginStep::Verification);
            Ok(())
        },
        || thread::sleep(POLL),
    )
}

fn await_product_context_with(
    cancel: &AtomicBool,
    timeout: Duration,
    mut observe: impl FnMut() -> Result<(), Reason>,
    mut show: impl FnMut() -> Result<(), LoginError>,
    mut wait: impl FnMut(),
) -> Result<(), LoginError> {
    let started = Instant::now();
    let mut shown = false;
    let mut diagnostics = LoginDiagnostics::default();
    loop {
        check_cancelled(cancel)?;
        let reason = match observe() {
            Ok(()) => return Ok(()),
            Err(reason) => reason,
        };
        check_cancelled(cancel)?;
        if reason == Reason::SessionEnded {
            return Err(LoginError::SessionEnded);
        }
        if reason == Reason::SessionActionRequired && !shown {
            show()?;
            shown = true;
        }
        diagnostics.waiting(reason);
        if started.elapsed() >= timeout {
            return Err(LoginError::Timeout);
        }
        wait();
    }
}

#[derive(Default)]
struct LoginDiagnostics(Option<Reason>);

impl LoginDiagnostics {
    fn waiting(&mut self, reason: Reason) {
        if self.0 != Some(reason) {
            logging::record(Event::LoginWaiting, reason);
            self.0 = Some(reason);
        }
    }
}

/// The Riot Client's local HTTPS API, described by its lockfile.
struct LocalApi {
    base: String,
    password: String,
    client: reqwest::blocking::Client,
}

impl LocalApi {
    fn read() -> Option<Self> {
        let lockfile = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)?
            .join(r"Riot Games\Riot Client\Config\lockfile");
        let contents = std::fs::read_to_string(lockfile).ok()?;
        Self::from_lockfile(&contents)
    }

    fn from_lockfile(contents: &str) -> Option<Self> {
        // name:pid:port:password:protocol
        let parts: Vec<&str> = contents.trim().split(':').collect();
        let [_, _, port, password, _] = parts[..] else {
            return None;
        };
        let client = reqwest::blocking::Client::builder()
            // The API is bound to loopback with a self-signed certificate.
            .danger_accept_invalid_certs(true)
            .no_proxy()
            .timeout(Duration::from_secs(3))
            .build()
            .ok()?;
        Some(Self {
            base: format!("https://127.0.0.1:{}", port.parse::<u16>().ok()?),
            password: password.to_owned(),
            client,
        })
    }

    fn functions(&self) -> Option<HashSet<String>> {
        let body = self
            .client
            .get(format!("{}/help", self.base))
            .basic_auth("riot", Some(&self.password))
            .send()
            .ok()?
            .text()
            .ok()?;
        let help: serde_json::Value = serde_json::from_str(&body).ok()?;
        Some(help["functions"].as_object()?.keys().cloned().collect())
    }

    fn get(&self, path: &str) -> Option<(u16, serde_json::Value)> {
        let response = self
            .client
            .get(format!("{}{path}", self.base))
            .basic_auth("riot", Some(&self.password))
            .send()
            .ok()?;
        let status = response.status().as_u16();
        let body = response.text().ok().unwrap_or_default();
        Some((status, serde_json::from_str(&body).unwrap_or_default()))
    }

    fn session(&self) -> Session {
        session_state(self.get("/player-session-lifecycle/v1/session"))
    }

    /// Ask the client to launch a product.
    fn launch(&self, game: Game) -> Launch {
        let response = self
            .client
            .post(format!(
                "{}/product-launcher/v1/products/{}/patchlines/live",
                self.base,
                game.product()
            ))
            .basic_auth("riot", Some(&self.password))
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body("{}")
            .send();
        match response {
            // 423: an earlier request already launched it.
            Ok(response) if response.status().is_success() || response.status() == 423 => {
                Launch::Accepted
            }
            Err(error) if error.is_timeout() => Launch::Pending,
            _ => Launch::Rejected,
        }
    }

    /// The game signs in to the region the Riot Client is set to. Riot's
    /// window moves it to the account's League region after each sign-in;
    /// without that window, an account from another region would start
    /// League in the previous account's region and fail to log in. An
    /// unreadable state changes nothing.
    fn follow_league_region(
        &self,
        timeout: Duration,
        cancel: &AtomicBool,
    ) -> Result<(), LoginError> {
        const REGION_LOCALE: &str = "/riotclient/region-locale";
        let (Some(info), Some((200, current)), Some((200, regions))) = (
            self.userinfo(),
            self.get(REGION_LOCALE),
            self.get("/data-store/v1/system-settings/region_data"),
        ) else {
            return Ok(());
        };
        let Some(region) = current["region"]
            .as_str()
            .and_then(|current| league_region(&info, &regions, current))
        else {
            return Ok(());
        };
        check_cancelled(cancel)?;
        logging::record(Event::LoginWaiting, Reason::RegionChange);
        // The same request as Riot's window; the result is read back below.
        let _ = self
            .client
            .put(format!("{}{REGION_LOCALE}", self.base))
            .basic_auth("riot", Some(&self.password))
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(serde_json::json!({"region": region, "locale": current["locale"]}).to_string())
            .send();
        let started = Instant::now();
        loop {
            check_cancelled(cancel)?;
            if self
                .get(REGION_LOCALE)
                .is_some_and(|(status, body)| status == 200 && body["region"] == region)
            {
                return Ok(());
            }
            if started.elapsed() >= timeout {
                return Err(LoginError::RegionFailed);
            }
            thread::sleep(POLL);
        }
    }

    /// Riot account information of the signed-in account.
    fn userinfo(&self) -> Option<serde_json::Value> {
        let (status, body) = self.get("/rso-auth/v1/authorization/userinfo")?;
        if status != 200 {
            return None;
        }
        // `userInfo` is itself a JSON document encoded as a string.
        match &body["userInfo"] {
            serde_json::Value::String(text) => serde_json::from_str(text).ok(),
            other => Some(other.clone()),
        }
    }

    /// Username of the signed-in account, if any.
    fn username(&self) -> Option<String> {
        self.userinfo()?["username"]
            .as_str()
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
    }

    /// Read the game client's own session, not the Riot launcher's session.
    fn game_username(&self) -> Option<String> {
        let (status, body) = self.get("/lol-login/v1/session")?;
        if status != 200 || body["state"] != "SUCCEEDED" {
            return None;
        }
        body["username"]
            .as_str()
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
    }

    fn sign_out(&self, cancel: &AtomicBool) -> Result<(), LoginError> {
        check_cancelled(cancel)?;
        if !self
            .functions()
            .is_some_and(|functions| functions.contains("DeletePlayerSessionLifecycleV1Session"))
        {
            return Err(LoginError::SignOutFailed);
        }
        check_cancelled(cancel)?;
        let sent = self
            .client
            .delete(format!("{}/player-session-lifecycle/v1/session", self.base))
            .basic_auth("riot", Some(&self.password))
            .send()
            .is_ok_and(|response| response.status().is_success());
        if sent {
            Ok(())
        } else {
            Err(LoginError::SignOutFailed)
        }
    }
}

/// The Riot Client region (a `region_data` key such as `EUW`) to switch to so
/// that League signs in to one of the account's platforms (such as `EUW1`).
/// Like Riot's window, keep a region the account plays on, otherwise take
/// the account's current platform. `None` when nothing needs to change or
/// the account has no known League platform.
fn league_region(
    info: &serde_json::Value,
    regions: &serde_json::Value,
    current: &str,
) -> Option<String> {
    let platforms: Vec<&str> = std::iter::once(&info["lol"])
        .chain(info["lol_region"].as_array().into_iter().flatten())
        .filter(|account| account["active"] != false)
        .filter_map(|account| account["cpid"].as_str())
        .collect();
    let platform = *platforms.first()?;
    let platform_of = |region: &str| regions[region]["rso_platform_id"].as_str();
    if platform_of(current).is_some_and(|current| platforms.contains(&current)) {
        return None;
    }
    // Some regions are named after their platform (KR); most are not (EUW).
    let region = if platform_of(platform).is_some() {
        platform.to_owned()
    } else {
        regions
            .as_object()?
            .iter()
            .find(|(_, data)| data["rso_platform_id"] == platform)?
            .0
            .clone()
    };
    (region != current).then_some(region)
}

/// The League suspension in Riot's userinfo `ban.restrictions`, if any.
/// Chat, ranked and reputation restrictions still allow playing.
fn league_ban(info: &serde_json::Value, now_millis: u64) -> Option<Ban> {
    let mut ban: Option<Ban> = None;
    for restriction in info["ban"]["restrictions"].as_array()? {
        if restriction["scope"] != "lol" {
            continue;
        }
        let until = match restriction["type"].as_str() {
            Some("PERMANENT_BAN") => None,
            Some("TIME_BAN") => match restriction["dat"]["expirationMillis"].as_u64() {
                Some(until) if until > now_millis => Some(until),
                _ => continue,
            },
            _ => continue,
        };
        // A permanent ban outlasts any timed one; otherwise the latest end wins.
        ban = Some(Ban {
            until: match (ban, until) {
                (Some(Ban { until: None }), _) | (_, None) => None,
                (Some(Ban { until: Some(other) }), Some(until)) => Some(other.max(until)),
                (None, until) => until,
            },
        });
    }
    ban
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as u64)
}

/// Hand the League suspension Riot reports for the signed-in account to
/// `on_ban`. Riot disconnects a suspended account as soon as the game starts,
/// so stop before launching one unless `on_ban` returns true. An unreadable
/// userinfo is not reported and does not block the login.
fn check_ban(
    game: Option<Game>,
    on_ban: &mut dyn FnMut(Option<Ban>) -> bool,
) -> Result<(), LoginError> {
    let Some(info) = LocalApi::read().and_then(|api| api.userinfo()) else {
        return Ok(());
    };
    let ban = league_ban(&info, now_millis());
    let launch_banned = on_ban(ban);
    match ban {
        Some(ban) if game.is_some() && !launch_banned => Err(LoginError::Banned(ban)),
        _ => Ok(()),
    }
}

/// League/TFT client processes (closing them only ends the lobby).
const CLIENT_PROCESSES: [&str; 4] = [
    "leagueclient.exe",
    "leagueclientux.exe",
    "leagueclientuxrender.exe",
    "tftclient.exe",
];
/// The in-match game process (closing it abandons the match).
const GAME_PROCESS: &str = "league of legends.exe";

struct Process {
    name: String,
    pid: u32,
}

fn running_processes() -> Vec<Process> {
    let mut command = std::process::Command::new("tasklist");
    command.args(["/FO", "CSV", "/NH"]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let Ok(output) = command.output() else {
        return Vec::new();
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let mut fields = line.split("\",\"");
            Some(Process {
                name: fields.next()?.trim_matches('"').to_ascii_lowercase(),
                pid: fields.next()?.trim_matches('"').parse().ok()?,
            })
        })
        .collect()
}

/// Whether a match is being played, so an update restart can wait for it.
pub fn in_game() -> bool {
    running_processes().iter().any(|process| process.name == GAME_PROCESS)
}

fn game_username(processes: &[Process]) -> Option<String> {
    let mut username: Option<String> = None;
    for process in processes
        .iter()
        .filter(|process| matches!(process.name.as_str(), "leagueclient.exe" | "tftclient.exe"))
    {
        let executable = windows::process_path(process.pid)?;
        let contents = std::fs::read_to_string(executable.parent()?.join("lockfile")).ok()?;
        // Do not trust a lockfile left by a different process or installation.
        if contents.split(':').nth(1)?.parse::<u32>().ok()? != process.pid {
            return None;
        }
        let name = LocalApi::from_lockfile(&contents)?.game_username()?;
        if username
            .as_ref()
            .is_some_and(|previous| !previous.eq_ignore_ascii_case(&name))
        {
            return None;
        }
        username = Some(name);
    }
    username
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GameStatus {
    pub client_open: bool,
    pub in_game: bool,
    /// Username signed in to the Riot Client, when the API answers.
    pub signed_in_user: Option<String>,
    /// Username independently confirmed by every running game client's API.
    pub game_user: Option<String>,
}

impl GameStatus {
    pub fn league_running(&self) -> bool {
        self.client_open || self.in_game
    }

    pub fn signed_in_as(&self, account_id: &str) -> bool {
        let matches = |name: &Option<String>| {
            name.as_deref()
                .is_some_and(|name| !name.is_empty() && name.eq_ignore_ascii_case(account_id))
        };
        matches(&self.signed_in_user) && (!self.league_running() || matches(&self.game_user))
    }
}

pub fn status() -> GameStatus {
    let processes = running_processes();
    let signed_in_user = LocalApi::read()
        .filter(|api| api.session() == Session::SignedIn)
        .and_then(|api| api.username());
    GameStatus {
        client_open: processes
            .iter()
            .any(|process| CLIENT_PROCESSES.contains(&process.name.as_str())),
        in_game: processes.iter().any(|process| process.name == GAME_PROCESS),
        signed_in_user,
        game_user: game_username(&processes),
    }
}

/// Close the League/TFT client and game: politely first, then forcefully.
fn close_league(cancel: &AtomicBool) -> Result<(), LoginError> {
    let names: Vec<&str> = CLIENT_PROCESSES
        .iter()
        .copied()
        .chain([GAME_PROCESS])
        .collect();
    let taskkill = |force: bool| {
        let mut command = std::process::Command::new("taskkill");
        if force {
            command.arg("/F");
        }
        for name in &names {
            command.args(["/IM", name]);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000);
        }
        let _ = command.output();
    };
    let closed = || {
        let processes = running_processes();
        !processes
            .iter()
            .any(|process| names.contains(&process.name.as_str()))
    };
    close_league_with(cancel, taskkill, closed, || {
        thread::sleep(Duration::from_millis(500));
    })
}

fn check_cancelled(cancel: &AtomicBool) -> Result<(), LoginError> {
    if cancel.load(Ordering::SeqCst) {
        Err(LoginError::Cancelled)
    } else {
        Ok(())
    }
}

fn close_league_with(
    cancel: &AtomicBool,
    mut taskkill: impl FnMut(bool),
    mut closed: impl FnMut() -> bool,
    mut wait: impl FnMut(),
) -> Result<(), LoginError> {
    check_cancelled(cancel)?;
    taskkill(false);
    for attempt in 0..20 {
        wait();
        check_cancelled(cancel)?;
        if closed() {
            return Ok(());
        }
        if attempt == 10 {
            check_cancelled(cancel)?;
            taskkill(true);
        }
    }
    check_cancelled(cancel)?;
    if closed() {
        Ok(())
    } else {
        Err(LoginError::CloseFailed)
    }
}

/// Sign in through Riot's native form, then launch the requested game.
/// `on_ban` receives the League suspension Riot reports once signed in and
/// returns whether a suspended account may still launch the game.
pub fn login(
    account_id: &str,
    password: &str,
    game: Option<Game>,
    close_running: bool,
    progress: &mut dyn FnMut(LoginStep),
    on_ban: &mut dyn FnMut(Option<Ban>) -> bool,
    cancel: &AtomicBool,
) -> Result<LoginOutcome, LoginError> {
    check_cancelled(cancel)?;
    let path = client_path().ok_or(LoginError::ClientMissing)?;
    let current = status();
    check_cancelled(cancel)?;
    if current.league_running() && (close_running || !current.signed_in_as(account_id)) {
        if !close_running {
            return Err(LoginError::LeagueRunning);
        }
        progress(LoginStep::CloseLeague);
        close_league(cancel)?;
    }
    // Otherwise a running game already belongs to this account.
    let game_open = current.league_running() && !close_running;
    // Keep startup verification visible; temporary renderer ownership starts
    // only after the lifecycle confirms that the client is signed out.
    let mut client = renderer::Renderer::new(&path);
    progress(LoginStep::OpenClient);
    client.ensure_service(cancel)?;
    progress(LoginStep::WaitAuth);
    let started = Instant::now();
    let mut sign_out_sent: Option<Instant> = None;
    let mut waiting_step = LoginStep::WaitAuth;
    let mut diagnostics = LoginDiagnostics::default();
    loop {
        check_cancelled(cancel)?;
        if started.elapsed() > LOGIN_TIMEOUT {
            return Err(LoginError::Timeout);
        }
        if let Some(api) = LocalApi::read() {
            match api.session() {
                Session::SignedIn => match api.username() {
                    Some(name) if name.eq_ignore_ascii_case(account_id) => {
                        client.restore()?;
                        check_cancelled(cancel)?;
                        check_ban(game, on_ban)?;
                        if let Some(game) = game {
                            if !game_open {
                                await_product_context(&mut client, progress, cancel)?;
                            }
                            progress(LoginStep::LaunchGame);
                            start_game(game, account_id, cancel)?;
                        }
                        progress(LoginStep::Done);
                        return Ok(LoginOutcome::AlreadySignedIn);
                    }
                    Some(_) if sign_out_sent.is_none() => {
                        // Signing in needs a client this app can restart; do
                        // not sign the other account out without one.
                        client.ensure_controllable()?;
                        progress(LoginStep::SignOut);
                        api.sign_out(cancel)?;
                        sign_out_sent = Some(Instant::now());
                    }
                    Some(_)
                        if sign_out_sent
                            .is_some_and(|sent| sent.elapsed() > Duration::from_secs(15)) =>
                    {
                        return Err(LoginError::SignOutFailed)
                    }
                    _ => diagnostics.waiting(Reason::AuthIdentityPending),
                },
                Session::SignedOut => break,
                Session::Unknown(reason) => {
                    let step = if reason == Reason::AuthInteraction {
                        LoginStep::Verification
                    } else {
                        LoginStep::WaitAuth
                    };
                    if step != waiting_step {
                        progress(step);
                        waiting_step = step;
                    }
                    diagnostics.waiting(reason);
                }
            }
        }
        thread::sleep(POLL);
    }
    renderer::sign_in(&mut client, account_id, password, progress, cancel)?;
    // Dropping `client` on a ban reopens the normal Riot window.
    check_ban(game, on_ban)?;
    if let Some(game) = game {
        await_product_context(&mut client, progress, cancel)?;
        // The game replaces the Riot window, so do not reopen it in between.
        // If the launch fails or is cancelled, dropping `client` reopens it.
        client.close_debugger()?;
        check_cancelled(cancel)?;
        progress(LoginStep::LaunchGame);
        start_game(game, account_id, cancel)?;
        client.replaced_by_game();
    } else {
        client.restore()?;
        check_cancelled(cancel)?;
    }
    progress(LoginStep::Done);
    Ok(LoginOutcome::SignedIn)
}

#[cfg(windows)]
mod windows {
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ACCESS_DENIED};
    use windows_sys::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
        PROCESS_TERMINATE,
    };

    /// Whether Windows refuses to let this process end `pid`, as for a process
    /// running as administrator when this one is not. An exited process is
    /// not refused.
    pub fn terminate_denied(pid: u32) -> bool {
        // SAFETY: plain terminate-right probe, closed below; nothing is ended.
        let process = unsafe { OpenProcess(PROCESS_TERMINATE, 0, pid) };
        if process.is_null() {
            // SAFETY: reads this thread's error from the call above.
            return unsafe { GetLastError() } == ERROR_ACCESS_DENIED;
        }
        // SAFETY: handle opened above.
        unsafe { CloseHandle(process) };
        false
    }

    pub fn process_path(pid: u32) -> Option<std::path::PathBuf> {
        // SAFETY: plain query handle, closed below.
        let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if process.is_null() {
            return None;
        }
        let mut buffer = [0u16; 1024];
        let mut length = buffer.len() as u32;
        // SAFETY: buffer and length describe a writable UTF-16 buffer.
        let ok =
            unsafe { QueryFullProcessImageNameW(process, 0, buffer.as_mut_ptr(), &mut length) };
        // SAFETY: handle opened above.
        unsafe { CloseHandle(process) };
        if ok == 0 {
            return None;
        }
        let path = String::from_utf16_lossy(&buffer[..length as usize]);
        Some(std::path::PathBuf::from(path))
    }
}

#[cfg(not(windows))]
mod windows {
    pub fn terminate_denied(_: u32) -> bool {
        false
    }

    pub fn process_path(_: u32) -> Option<std::path::PathBuf> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn lifecycle_never_treats_startup_errors_or_challenges_as_a_login_form() {
        use serde_json::json;

        for response in [
            None,
            Some((404, json!({}))),
            Some((503, json!({}))),
            Some((200, json!({}))),
        ] {
            assert_eq!(
                session_state(response),
                Session::Unknown(Reason::AuthUnavailable)
            );
        }
        for state in [
            "PendingInitialization",
            "RestoreAuthorization",
            "PendingClientConfigData",
            "FutureState",
        ] {
            assert_eq!(
                session_state(Some((200, json!({"loginState": state, "puuid": ""})))),
                Session::Unknown(Reason::AuthStarting)
            );
        }
        assert_eq!(
            session_state(Some((
                200,
                json!({
                    "loginState": "PendingAuthentication", "actionRequired": true
                })
            ))),
            Session::Unknown(Reason::AuthInteraction)
        );
        // An identity always prevents typing, even across a transitional response.
        assert_eq!(
            session_state(Some((
                200,
                json!({
                    "loginState": "PendingLoginStrategy", "puuid": "signed-in-player"
                })
            ))),
            Session::SignedIn
        );
    }

    #[test]
    fn only_a_finished_session_is_handed_to_the_game() {
        use serde_json::json;

        let signed_in = |state: &str, action: bool| {
            Some((
                200,
                json!({"loginState": state, "puuid": "player", "actionRequired": action}),
            ))
        };
        assert_eq!(
            product_context(signed_in("PendingProductContext", false)),
            Ok(())
        );
        // The identity appears during token exchange, before terms and flags.
        for state in [
            "LoginTokenExchange",
            "PendingEulaData",
            "PendingPersistentFlagsData",
            "FutureState",
        ] {
            assert_eq!(
                product_context(signed_in(state, false)),
                Err(Reason::SessionPending)
            );
        }
        for state in ["PendingEulaAcceptance", "PendingProductContext"] {
            assert_eq!(
                product_context(signed_in(state, true)),
                Err(Reason::SessionActionRequired)
            );
        }
        for response in [None, Some((503, json!({})))] {
            assert_eq!(product_context(response), Err(Reason::AuthUnavailable));
        }
        // The login form requires an action too, but nobody is signed in.
        assert_eq!(
            product_context(Some((
                200,
                json!({"loginState": "PendingLoginStrategy", "puuid": "", "actionRequired": true})
            ))),
            Err(Reason::SessionEnded)
        );
        // A transitional response that still carries the identity is not a sign-out.
        assert_eq!(
            product_context(signed_in("PendingLoginStrategy", false)),
            Err(Reason::SessionPending)
        );
    }

    #[test]
    fn a_session_waiting_for_the_player_shows_riot_once_before_the_launch() {
        let states = [
            Err(Reason::SessionPending),
            Err(Reason::SessionActionRequired),
            Err(Reason::SessionActionRequired),
            Err(Reason::SessionPending),
            Ok(()),
        ];
        let polls = Cell::new(0);
        let shown = Cell::new(0);
        let result = await_product_context_with(
            &AtomicBool::new(false),
            Duration::MAX,
            || {
                polls.set(polls.get() + 1);
                states[polls.get() - 1]
            },
            || {
                shown.set(shown.get() + 1);
                Ok(())
            },
            || {},
        );
        assert_eq!(result, Ok(()));
        assert_eq!(polls.get(), states.len());
        assert_eq!(shown.get(), 1);

        // The limit is time, however long each poll takes.
        let polls = Cell::new(0);
        let result = await_product_context_with(
            &AtomicBool::new(false),
            Duration::ZERO,
            || {
                polls.set(polls.get() + 1);
                Err(Reason::SessionPending)
            },
            || panic!("nothing needs the player"),
            || panic!("the time is up"),
        );
        assert_eq!(result, Err(LoginError::Timeout));
        assert_eq!(polls.get(), 1);

        // Signing out ends the wait at once instead of at the time limit.
        let states = [
            Err(Reason::SessionActionRequired),
            Err(Reason::SessionEnded),
        ];
        let polls = Cell::new(0);
        let result = await_product_context_with(
            &AtomicBool::new(false),
            Duration::MAX,
            || {
                polls.set(polls.get() + 1);
                states[polls.get() - 1]
            },
            || Ok(()),
            || {},
        );
        assert_eq!(result, Err(LoginError::SessionEnded));
        assert_eq!(polls.get(), states.len());

        let cancel = AtomicBool::new(false);
        let result = await_product_context_with(
            &cancel,
            Duration::MAX,
            || {
                cancel.store(true, Ordering::SeqCst);
                Err(Reason::SessionActionRequired)
            },
            || panic!("must not show Riot after cancellation"),
            || {},
        );
        assert_eq!(result, Err(LoginError::Cancelled));
    }

    #[test]
    fn cold_start_and_sign_out_use_the_lifecycle_without_waiting_for_rso_initialization() {
        use std::io::{BufRead, Write};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let responses = [
            (
                r#"{"loginState":"RestoreAuthorization","puuid":""}"#,
                Session::Unknown(Reason::AuthStarting),
            ),
            (
                r#"{"loginState":"PendingLoginStrategy","puuid":""}"#,
                Session::SignedOut,
            ),
            (
                r#"{"loginState":"PendingProductContext","puuid":"player"}"#,
                Session::SignedIn,
            ),
            (
                r#"{"loginState":"PendingLoginStrategy","puuid":""}"#,
                Session::SignedOut,
            ),
        ];
        let mut exchanges: Vec<_> = responses
            .iter()
            .map(|(body, _)| ("GET /player-session-lifecycle/v1/session ", *body))
            .collect();
        exchanges.splice(
            3..3,
            [
                (
                    "GET /help ",
                    r#"{"functions":{"DeletePlayerSessionLifecycleV1Session":{}}}"#,
                ),
                ("DELETE /player-session-lifecycle/v1/session ", "{}"),
            ],
        );
        let server = thread::spawn(move || {
            for (request, body) in exchanges {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut reader = std::io::BufReader::new(&mut stream);
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                // No legacy ready-state, authorization or RSO-session request:
                // those may remain uninitialized until the first manual login.
                assert!(line.starts_with(request));
                loop {
                    line.clear();
                    assert!(reader.read_line(&mut line).unwrap() > 0);
                    if line == "\r\n" {
                        break;
                    }
                }
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        });
        let api = LocalApi {
            base: format!("http://{address}"),
            password: "test".into(),
            client: reqwest::blocking::Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(5))
                .build()
                .unwrap(),
        };
        for (index, (_, expected)) in responses.into_iter().enumerate() {
            if index == 3 {
                assert_eq!(api.sign_out(&AtomicBool::new(false)), Ok(()));
            }
            assert_eq!(api.session(), expected);
        }
        server.join().unwrap();
    }

    #[test]
    #[cfg(windows)]
    fn process_discovery_locates_the_executable_for_a_running_pid() {
        let processes = running_processes();
        let current = processes
            .iter()
            .find(|process| process.pid == std::process::id())
            .unwrap();
        let executable = std::env::current_exe().unwrap();
        assert_eq!(
            current.name,
            executable
                .file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .to_ascii_lowercase()
        );
        assert_eq!(windows::process_path(current.pid), Some(executable));
    }

    #[test]
    #[cfg(windows)]
    fn only_a_refused_process_counts_as_out_of_reach() {
        assert!(!windows::terminate_denied(std::process::id()));
        // The protected System process refuses even administrators.
        assert!(windows::terminate_denied(4));
        // No process has this ID; one that exited is not refused either.
        assert!(!windows::terminate_denied(u32::MAX - 3));
    }

    #[test]
    fn only_current_league_suspensions_count_as_bans() {
        use serde_json::json;

        let restriction = |kind: &str, scope: &str, until: u64| {
            json!({
                "type": kind, "reason": "INAPPROPRIATE_TEXT", "scope": scope,
                "dat": {"expirationMillis": until, "gameData": {"productName": scope}}
            })
        };
        let info = |restrictions: Vec<serde_json::Value>| json!({"ban": {"restrictions": restrictions}});
        let now = 1_000;

        // Shape observed on a suspended account: chat limits sit beside the ban.
        let observed = info(vec![
            restriction("TEXT_CHAT_RESTRICTION", "lol", 9_000),
            restriction("REPUTATION_LIMIT", "lol", 0),
            restriction("TIME_BAN", "lol", 5_000),
            restriction("TEXT_CHAT_MUTED", "lol", 0),
        ]);
        assert_eq!(league_ban(&observed, now), Some(Ban { until: Some(5_000) }));

        assert_eq!(league_ban(&info(vec![]), now), None);
        assert_eq!(league_ban(&json!({}), now), None);
        assert_eq!(league_ban(&info(vec![restriction("TIME_BAN", "lol", 1_000)]), now), None);
        assert_eq!(league_ban(&info(vec![restriction("TIME_BAN", "valorant", 5_000)]), now), None);
        assert_eq!(
            league_ban(
                &info(vec![
                    restriction("TIME_BAN", "lol", 5_000),
                    restriction("TIME_BAN", "lol", 7_000),
                ]),
                now
            ),
            Some(Ban { until: Some(7_000) })
        );
        assert_eq!(
            league_ban(
                &info(vec![
                    restriction("PERMANENT_BAN", "lol", 0),
                    restriction("TIME_BAN", "lol", 7_000),
                ]),
                now
            ),
            Some(Ban { until: None })
        );
    }

    #[test]
    fn the_riot_region_follows_the_accounts_league_platform() {
        use serde_json::json;

        let regions = json!({
            "EUNE": {"rso_platform_id": "EUN1"},
            "EUW": {"rso_platform_id": "EUW1"},
            "KR": {"rso_platform_id": "KR"},
            "NA": {"rso_platform_id": "NA1"},
        });
        let account = |platforms: &[&str]| {
            let entries: Vec<_> = platforms
                .iter()
                .map(|platform| json!({"cpid": platform, "pid": platform, "active": true}))
                .collect();
            json!({"lol": entries.first(), "lol_region": entries})
        };
        let switch = |platforms: &[&str], current: &str| {
            league_region(&account(platforms), &regions, current)
        };

        // Issue #33: an EUW account after an EUNE one.
        assert_eq!(switch(&["EUW1"], "EUNE").as_deref(), Some("EUW"));
        assert_eq!(switch(&["EUW1"], "EUW"), None);
        assert_eq!(switch(&["KR"], "EUW").as_deref(), Some("KR"));
        // A region the account also plays on stays selected.
        assert_eq!(switch(&["EUW1", "EUN1"], "EUNE"), None);
        assert_eq!(switch(&["EUW1", "EUN1"], "NA").as_deref(), Some("EUW"));
        // An unknown client region is replaced as well.
        assert_eq!(switch(&["NA1"], "XX").as_deref(), Some("NA"));
        let moved = json!({
            "lol": {"cpid": "EUW1", "pid": "EUN1", "active": true},
            "lol_region": [{"cpid": "EUN1", "active": false}, {"cpid": "EUW1", "active": true}],
        });
        assert_eq!(
            league_region(&moved, &regions, "EUNE").as_deref(),
            Some("EUW")
        );

        // Without a known League platform, the client is left alone.
        assert_eq!(league_region(&json!({}), &regions, "EUNE"), None);
        assert_eq!(switch(&["PBE1"], "EUNE"), None);
        assert_eq!(league_region(&account(&["EUW1"]), &json!({}), "EUNE"), None);
    }

    /// Answers each request with the next body and returns the bodies sent.
    fn serve_local_api(
        exchanges: Vec<(&'static str, String)>,
    ) -> (LocalApi, thread::JoinHandle<Vec<String>>) {
        use std::io::{BufRead, Read, Write};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let mut requests = Vec::new();
            for (request, body) in exchanges {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut reader = std::io::BufReader::new(&mut stream);
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                assert!(line.starts_with(request), "{line}");
                let mut length = 0;
                loop {
                    let mut header = String::new();
                    assert!(reader.read_line(&mut header).unwrap() > 0);
                    if header == "\r\n" {
                        break;
                    }
                    if let Some((name, value)) = header.split_once(':') {
                        if name.eq_ignore_ascii_case("content-length") {
                            length = value.trim().parse().unwrap();
                        }
                    }
                }
                let mut sent = vec![0; length];
                reader.read_exact(&mut sent).unwrap();
                requests.push(String::from_utf8(sent).unwrap());
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
            requests
        });
        let api = LocalApi {
            base: format!("http://{address}"),
            password: "test".into(),
            client: reqwest::blocking::Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(5))
                .build()
                .unwrap(),
        };
        (api, server)
    }

    #[test]
    fn the_launch_waits_until_riot_reports_the_accounts_region() {
        use serde_json::json;

        let info = json!({
            "userInfo": json!({"lol": {"cpid": "EUW1", "active": true}}).to_string()
        })
        .to_string();
        let regions =
            json!({"EUNE": {"rso_platform_id": "EUN1"}, "EUW": {"rso_platform_id": "EUW1"}})
                .to_string();
        let locale = |region: &str| json!({"locale": "en_GB", "region": region}).to_string();
        let reads = |current: &str| {
            vec![
                ("GET /rso-auth/v1/authorization/userinfo ", info.clone()),
                ("GET /riotclient/region-locale ", locale(current)),
                (
                    "GET /data-store/v1/system-settings/region_data ",
                    regions.clone(),
                ),
            ]
        };

        let mut exchanges = reads("EUNE");
        exchanges.extend([
            ("PUT /riotclient/region-locale ", String::new()),
            ("GET /riotclient/region-locale ", locale("EUW")),
        ]);
        let (api, server) = serve_local_api(exchanges);
        let log = logging::capture(|| {
            assert_eq!(
                api.follow_league_region(Duration::MAX, &AtomicBool::new(false)),
                Ok(())
            );
        });
        let sent: serde_json::Value = serde_json::from_str(&server.join().unwrap()[3]).unwrap();
        assert_eq!(sent, json!({"region": "EUW", "locale": "en_GB"}));
        assert!(log.contains("INFO event=login_waiting reason=region_change"));

        // Already in the account's region: nothing is changed.
        let (api, server) = serve_local_api(reads("EUW"));
        assert_eq!(
            api.follow_league_region(Duration::MAX, &AtomicBool::new(false)),
            Ok(())
        );
        assert_eq!(server.join().unwrap().len(), 3);

        // A region that never applies stops the launch with its own error.
        let mut exchanges = reads("EUNE");
        exchanges.extend([
            ("PUT /riotclient/region-locale ", String::new()),
            ("GET /riotclient/region-locale ", locale("EUNE")),
        ]);
        let (api, server) = serve_local_api(exchanges);
        assert_eq!(
            api.follow_league_region(Duration::ZERO, &AtomicBool::new(false)),
            Err(LoginError::RegionFailed)
        );
        server.join().unwrap();
    }

    #[test]
    fn riot_identity_does_not_prove_the_open_games_identity() {
        let mut current = GameStatus {
            client_open: true,
            signed_in_user: Some("selected".into()),
            game_user: Some("previous".into()),
            ..GameStatus::default()
        };
        assert!(!current.signed_in_as("selected"));
        assert_eq!(
            launch_confirmed(&current, "selected"),
            Err(LoginError::AccountMismatch)
        );
        current.game_user = None;
        assert!(!current.signed_in_as("selected"));
        assert_eq!(launch_confirmed(&current, "selected"), Ok(false));
        current.game_user = Some("SELECTED".into());
        assert!(current.signed_in_as("selected"));
        assert_eq!(launch_confirmed(&current, "selected"), Ok(true));
        current.signed_in_user = None;
        assert_eq!(launch_confirmed(&current, "selected"), Ok(false));
        current.signed_in_user = Some("previous".into());
        assert_eq!(
            launch_confirmed(&current, "selected"),
            Err(LoginError::AccountMismatch)
        );
        current.client_open = false;
        current.in_game = true;
        current.signed_in_user = Some("selected".into());
        current.game_user = None;
        assert!(!current.signed_in_as("selected"));
    }

    #[test]
    fn launch_waits_for_the_game_account_and_stops_on_mismatch_or_cancellation() {
        for (game_user, expected) in [
            (Some("previous"), Err(LoginError::AccountMismatch)),
            (None, Err(LoginError::GameLaunchFailed)),
            (Some("selected"), Ok(())),
        ] {
            let cancel = AtomicBool::new(false);
            let polls = Cell::new(0);
            let requests = Cell::new(0);
            let result = launch_game_with(
                "selected",
                &cancel,
                || {
                    let poll = polls.get();
                    polls.set(poll + 1);
                    GameStatus {
                        client_open: poll > 0,
                        signed_in_user: Some("selected".into()),
                        game_user: if poll > 0 {
                            game_user.map(str::to_owned)
                        } else {
                            None
                        },
                        ..GameStatus::default()
                    }
                },
                || {
                    requests.set(requests.get() + 1);
                    Ok(Launch::Accepted)
                },
                || {},
            );
            assert_eq!(result, expected);
            assert_eq!(requests.get(), 1);
            assert!(polls.get() > 1);
        }

        // An unanswered request is not a refusal; asking again is harmless
        // because Riot answers once the first launch has started the game.
        let polls = Cell::new(0);
        let requests = Cell::new(0);
        let log = logging::capture(|| {
            let result = launch_game_with(
                "selected",
                &AtomicBool::new(false),
                || {
                    polls.set(polls.get() + 1);
                    GameStatus {
                        client_open: polls.get() > 4,
                        signed_in_user: Some("selected".into()),
                        game_user: (polls.get() > 4).then(|| "selected".into()),
                        ..GameStatus::default()
                    }
                },
                || {
                    requests.set(requests.get() + 1);
                    Ok(if requests.get() == 1 {
                        Launch::Pending
                    } else {
                        Launch::Accepted
                    })
                },
                || {},
            );
            assert_eq!(result, Ok(()));
        });
        assert_eq!(requests.get(), 2);
        assert!(log.contains("reason=launch_pending"));
        assert!(!log.contains("reason=launch_rejected"));

        let cancel = AtomicBool::new(false);
        let result = launch_game_with(
            "selected",
            &cancel,
            || {
                cancel.store(true, Ordering::SeqCst);
                GameStatus {
                    signed_in_user: Some("selected".into()),
                    ..GameStatus::default()
                }
            },
            || panic!("must not launch after cancellation"),
            || {},
        );
        assert_eq!(result, Err(LoginError::Cancelled));

        let cancel = AtomicBool::new(false);
        let result = launch_game_with(
            "selected",
            &cancel,
            || GameStatus {
                client_open: true,
                signed_in_user: Some("selected".into()),
                game_user: Some("previous".into()),
                ..GameStatus::default()
            },
            || panic!("must not launch over another account"),
            || {},
        );
        assert_eq!(result, Err(LoginError::AccountMismatch));
    }

    #[test]
    fn game_session_requires_successful_authentication_and_a_nonempty_username() {
        use std::io::{BufRead, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let responses = [
            (
                200,
                r#"{"state":"SUCCEEDED","username":"selected"}"#,
                Some("selected"),
            ),
            (
                200,
                r#"{"state":"IN_PROGRESS","username":"selected"}"#,
                None,
            ),
            (200, r#"{"state":"ERROR","username":"selected"}"#, None),
            (200, r#"{"state":"SUCCEEDED","username":""}"#, None),
            (200, r#"{"state":"SUCCEEDED"}"#, None),
            (503, r#"{"state":"SUCCEEDED","username":"selected"}"#, None),
        ];
        let server = thread::spawn(move || {
            for (status, body, _) in responses {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut reader = std::io::BufReader::new(&mut stream);
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                assert!(line.starts_with("GET /lol-login/v1/session "));
                loop {
                    line.clear();
                    assert!(reader.read_line(&mut line).unwrap() > 0);
                    if line == "\r\n" {
                        break;
                    }
                }
                write!(stream, "HTTP/1.1 {status} Response\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            }
        });
        let api = LocalApi {
            base: format!("http://{address}"),
            password: "test".into(),
            client: reqwest::blocking::Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(5))
                .build()
                .unwrap(),
        };
        for (_, _, expected) in responses {
            assert_eq!(api.game_username().as_deref(), expected);
        }
        server.join().unwrap();
    }

    #[test]
    fn login_diagnostics_distinguish_failure_success_and_cancellation() {
        let log = logging::capture(|| {
            let mut diagnostics = LoginDiagnostics::default();
            for reason in [
                Reason::AuthUnavailable,
                Reason::AuthStarting,
                Reason::AuthInteraction,
                Reason::AuthIdentityPending,
                Reason::SessionPending,
                Reason::SessionActionRequired,
                Reason::FindWindow,
                Reason::LoginFormReady,
                Reason::GameIdentityPending,
                Reason::GameProcessPending,
                Reason::LaunchPending,
                Reason::LaunchRejected,
                Reason::RegionChange,
            ] {
                diagnostics.waiting(reason);
                diagnostics.waiting(reason);
            }
            LoginStep::LaunchGame.record();
            record_login_result(&Err(LoginError::AccountMismatch));
            record_login_result(&Err(LoginError::GameLaunchFailed));
            record_login_result(&Err(LoginError::SignOutFailed));
            record_login_result(&Err(LoginError::SessionEnded));
            record_login_result(&Err(LoginError::RegionFailed));
            record_login_result(&Err(LoginError::RiotElevated));
            record_login_result(&Err(LoginError::RiotCloseFailed));
            record_login_result(&Err(LoginError::CloseFailed));
            record_login_result(&Err(LoginError::Cancelled));
            record_login_result(&Ok(LoginOutcome::AlreadySignedIn));
        });
        for entry in [
            "INFO event=login_waiting reason=auth_unavailable",
            "INFO event=login_waiting reason=auth_starting",
            "INFO event=login_waiting reason=auth_interaction",
            "INFO event=login_waiting reason=auth_identity_pending",
            "INFO event=login_waiting reason=session_pending",
            "INFO event=login_waiting reason=session_action_required",
            "INFO event=login_waiting reason=find_window",
            "INFO event=login_waiting reason=login_form_ready",
            "INFO event=login_waiting reason=game_identity_pending",
            "INFO event=login_waiting reason=game_process_pending",
            "INFO event=login_waiting reason=launch_pending",
            "INFO event=login_waiting reason=launch_rejected",
            "INFO event=login_waiting reason=region_change",
            "INFO event=login_progress reason=launch_game",
            "ERROR event=login_failed reason=account_mismatch",
            "ERROR event=login_failed reason=game_launch_failed",
            "ERROR event=login_failed reason=sign_out_failed",
            "ERROR event=login_failed reason=session_ended",
            "ERROR event=login_failed reason=region_failed",
            "ERROR event=login_failed reason=riot_elevated",
            "ERROR event=login_failed reason=riot_close_failed",
            "ERROR event=login_failed reason=close_failed",
            "INFO event=login_cancelled reason=cancelled",
            "INFO event=login_completed reason=already_signed_in",
        ] {
            assert_eq!(log.matches(entry).count(), 1, "{entry}");
        }
    }

    #[test]
    fn cancellation_during_endpoint_discovery_prevents_sign_out() {
        use std::io::{BufRead, Write};
        use std::sync::Arc;

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let server_cancel = Arc::clone(&cancel);
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut reader = std::io::BufReader::new(&mut stream);
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            assert!(line.starts_with("GET /help "));
            loop {
                line.clear();
                assert!(reader.read_line(&mut line).unwrap() > 0);
                if line == "\r\n" {
                    break;
                }
            }
            server_cancel.store(true, Ordering::SeqCst);
            let body = r#"{"functions":{"DeletePlayerSessionLifecycleV1Session":{}}}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        });
        let api = LocalApi {
            base: format!("http://{address}"),
            password: "test".into(),
            client: reqwest::blocking::Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(5))
                .build()
                .unwrap(),
        };
        assert_eq!(api.sign_out(&cancel), Err(LoginError::Cancelled));
        server.join().unwrap();
    }

    #[test]
    fn cancelled_close_does_not_touch_processes() {
        let cancel = AtomicBool::new(true);
        let result = close_league_with(
            &cancel,
            |_| panic!("cancelled close must not send process requests"),
            || panic!("cancelled close must not query processes"),
            || panic!("cancelled close must not wait"),
        );
        assert_eq!(result, Err(LoginError::Cancelled));
    }

    #[test]
    fn cancellation_before_escalation_prevents_forced_termination() {
        let cancel = AtomicBool::new(false);
        let checks = Cell::new(0);
        let mut requests = Vec::new();
        let result = close_league_with(
            &cancel,
            |force| requests.push(force),
            || {
                checks.set(checks.get() + 1);
                if checks.get() == 11 {
                    cancel.store(true, Ordering::SeqCst);
                }
                false
            },
            || {},
        );
        assert_eq!(result, Err(LoginError::Cancelled));
        assert_eq!(requests, vec![false]);
    }

    #[test]
    fn uncancelled_close_can_escalate_and_confirm_exit() {
        let cancel = AtomicBool::new(false);
        let forced = Cell::new(false);
        let mut requests = Vec::new();
        let result = close_league_with(
            &cancel,
            |force| {
                requests.push(force);
                forced.set(force);
            },
            || forced.get(),
            || {},
        );
        assert_eq!(result, Ok(()));
        assert_eq!(requests, vec![false, true]);
    }
}
