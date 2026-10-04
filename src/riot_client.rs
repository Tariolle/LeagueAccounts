//! Riot Client integration: locate and open the client, read its sign-in
//! state from the local API, type credentials into its login window, and
//! launch a game once sign-in is confirmed.
//!
//! Credentials are only typed after the auth service is ready, has reported
//! "signed out" continuously for `SIGNED_OUT_STABLE`, and the client's main
//! window is visible and focused. While the client starts, a session kept by
//! the background service can briefly look signed out before it is restored,
//! so a single "signed out" reading is never enough. The game is launched
//! only after the API confirms the intended account is signed in.

use crate::autotype::{type_credentials, wait_for_shortcut_release};
use crate::logging::{self, Event, Reason};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

const LOGIN_TIMEOUT: Duration = Duration::from_secs(60);
/// After typing, how long to wait for sign-in (2FA or captcha may need the user).
const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(90);
const SIGNED_OUT_STABLE: Duration = Duration::from_secs(4);
const WINDOW_STABLE: Duration = Duration::from_secs(2);
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
    /// Credentials were typed and the account is now signed in.
    SignedIn,
    /// Credentials were typed but sign-in was not confirmed in time
    /// (for example the user still has to enter a 2FA code).
    Typed,
    /// This account was already signed in; nothing was typed.
    AlreadySignedIn,
    /// A different account is signed in; nothing was typed.
    OtherAccount,
}

/// Progress reported to the UI while signing in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoginStep {
    CloseLeague,
    OpenClient,
    WaitAuth,
    SignOut,
    FindWindow,
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
    /// The login window or sign-in state never became observable.
    Timeout,
    /// Signing out of the current session is not available.
    SignOutFailed,
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
    WorkerPanicked,
}

impl LoginError {
    pub fn code(self) -> &'static str {
        match self {
            LoginError::ClientMissing => "riot_client_missing",
            LoginError::LaunchFailed => "riot_launch_failed",
            LoginError::Timeout => "riot_timeout",
            LoginError::SignOutFailed => "riot_sign_out_failed",
            LoginError::TypingFailed => "autotype_failed",
            LoginError::Cancelled => "login_cancelled",
            LoginError::LeagueRunning => "league_running",
            LoginError::CloseFailed => "league_close_failed",
            LoginError::GameLaunchFailed => "game_launch_failed",
            LoginError::AccountMismatch => "game_account_mismatch",
            LoginError::WorkerPanicked => "autotype_failed",
        }
    }
}

/// Only fixed categories cross the logging boundary; never account identifiers.
pub fn record_login_result(result: &Result<LoginOutcome, LoginError>) {
    let (event, reason) = match result {
        Ok(outcome) => (
            Event::LoginCompleted,
            match outcome {
                LoginOutcome::SignedIn => Reason::SignedIn,
                LoginOutcome::Typed => Reason::Typed,
                LoginOutcome::AlreadySignedIn => Reason::AlreadySignedIn,
                LoginOutcome::OtherAccount => Reason::OtherAccount,
            },
        ),
        Err(error) => (
            if *error == LoginError::Cancelled {
                Event::LoginCancelled
            } else {
                Event::LoginFailed
            },
            match error {
                LoginError::ClientMissing => Reason::ClientMissing,
                LoginError::LaunchFailed => Reason::LaunchFailed,
                LoginError::Timeout => Reason::Timeout,
                LoginError::SignOutFailed => Reason::SignOutFailed,
                LoginError::TypingFailed => Reason::TypingFailed,
                LoginError::Cancelled => Reason::Cancelled,
                LoginError::LeagueRunning => Reason::LeagueRunning,
                LoginError::CloseFailed => Reason::CloseFailed,
                LoginError::GameLaunchFailed => Reason::GameLaunchFailed,
                LoginError::AccountMismatch => Reason::AccountMismatch,
                LoginError::WorkerPanicked => Reason::WorkerPanicked,
            },
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

fn open_client(path: &PathBuf) -> bool {
    std::process::Command::new(path).spawn().is_ok()
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
                return Ok(false);
            };
            let Some(name) = api.username() else {
                return Ok(false);
            };
            check_cancelled(cancel)?;
            if !name.eq_ignore_ascii_case(account_id) {
                return Err(LoginError::AccountMismatch);
            }
            Ok(api.launch(game))
        },
        || thread::sleep(Duration::from_secs(1)),
    )
}

fn launch_game_with(
    account_id: &str,
    cancel: &AtomicBool,
    mut observe: impl FnMut() -> GameStatus,
    mut request: impl FnMut() -> Result<bool, LoginError>,
    mut wait: impl FnMut(),
) -> Result<(), LoginError> {
    let mut requested = false;
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
        if !requested
            && !current.league_running()
            && current.signed_in_as(account_id)
            && attempt % 2 == 0
        {
            requested = request()?;
        }
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
    Unknown,
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
        // Until the auth service reports ready, "not authorized" may only
        // mean the stored session has not been restored yet.
        let ready = self
            .get("/rso-auth/configuration/v3/ready-state")
            .is_some_and(|(status, body)| status == 200 && body["ready"] == true);
        if !ready {
            return Session::Unknown;
        }
        let Some((authorization, _)) = self.get("/rso-auth/v1/authorization") else {
            return Session::Unknown;
        };
        let session_type = self
            .get("/rso-auth/v1/session")
            .and_then(|(_, body)| body["type"].as_str().map(str::to_owned));
        match (authorization, session_type.as_deref()) {
            (200, _) | (_, Some("authenticated")) => Session::SignedIn,
            (404, _) => Session::SignedOut,
            _ => Session::Unknown,
        }
    }

    /// Ask the client to launch a product; true when the request is accepted.
    fn launch(&self, game: Game) -> bool {
        self.client
            .post(format!(
                "{}/product-launcher/v1/products/{}/patchlines/live",
                self.base,
                game.product()
            ))
            .basic_auth("riot", Some(&self.password))
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body("{}")
            .send()
            .is_ok_and(|response| response.status().is_success())
    }

    /// Username of the signed-in account, if any.
    fn username(&self) -> Option<String> {
        let (status, body) = self.get("/rso-auth/v1/authorization/userinfo")?;
        if status != 200 {
            return None;
        }
        // `userInfo` is itself a JSON document encoded as a string.
        let info: serde_json::Value = match &body["userInfo"] {
            serde_json::Value::String(text) => serde_json::from_str(text).ok()?,
            other => other.clone(),
        };
        info["username"]
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
            .is_some_and(|functions| functions.contains("DeleteRsoAuthV1Session"))
        {
            return Err(LoginError::SignOutFailed);
        }
        check_cancelled(cancel)?;
        let sent = self
            .client
            .delete(format!("{}/rso-auth/v1/session", self.base))
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
    let names: Vec<&str> = CLIENT_PROCESSES.iter().copied().chain([GAME_PROCESS]).collect();
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

/// Open the Riot Client and sign in with the credentials, then launch `game`
/// (when given) once the account is confirmed signed in.
///
/// If another account is signed in, nothing is typed and `OtherAccount` is
/// returned, unless `switch_account` asks to sign it out first.
#[allow(clippy::too_many_arguments)]
pub fn login(
    account_id: &str,
    password: &str,
    game: Option<Game>,
    switch_account: bool,
    close_running: bool,
    progress: &mut dyn FnMut(LoginStep),
    cancel: &AtomicBool,
) -> Result<LoginOutcome, LoginError> {
    let cancelled = || cancel.load(Ordering::SeqCst);
    check_cancelled(cancel)?;
    let path = client_path().ok_or(LoginError::ClientMissing)?;

    // Never sign another account in under an open League client or match.
    let current = status();
    check_cancelled(cancel)?;
    if current.league_running() && (close_running || !current.signed_in_as(account_id)) {
        if !close_running {
            return Err(LoginError::LeagueRunning);
        }
        progress(LoginStep::CloseLeague);
        close_league(cancel)?;
    }
    progress(LoginStep::OpenClient);
    check_cancelled(cancel)?;
    if !open_client(&path) {
        return Err(LoginError::LaunchFailed);
    }
    progress(LoginStep::WaitAuth);
    let mut reported = LoginStep::WaitAuth;
    let mut report = |step: LoginStep, progress: &mut dyn FnMut(LoginStep)| {
        if step != reported {
            reported = step;
            progress(step);
        }
    };
    let is_this_account =
        |api: &LocalApi| api.username().is_some_and(|name| name.eq_ignore_ascii_case(account_id));

    let started = Instant::now();
    let mut sign_out_sent: Option<Instant> = None;
    let mut signed_out_since: Option<Instant> = None;
    let mut window_since = None;
    let window = loop {
        if cancelled() {
            return Err(LoginError::Cancelled);
        }
        if started.elapsed() > LOGIN_TIMEOUT {
            return Err(LoginError::Timeout);
        }
        // The lockfile is rewritten when the client restarts; re-read it.
        let api = LocalApi::read();
        let session = api.as_ref().map_or(Session::Unknown, LocalApi::session);
        check_cancelled(cancel)?;
        if session != Session::SignedOut {
            signed_out_since = None;
        }
        match (session, api) {
            (Session::SignedIn, Some(api)) if is_this_account(&api) => {
                if let Some(game) = game {
                    report(LoginStep::LaunchGame, progress);
                    start_game(game, account_id, cancel)?;
                }
                progress(LoginStep::Done);
                return Ok(LoginOutcome::AlreadySignedIn);
            }
            (Session::SignedIn, _) if !switch_account => return Ok(LoginOutcome::OtherAccount),
            (Session::SignedIn, api) => match sign_out_sent {
                None => {
                    report(LoginStep::SignOut, progress);
                    api.ok_or(LoginError::SignOutFailed)?.sign_out(cancel)?;
                    sign_out_sent = Some(Instant::now());
                }
                Some(sent) if sent.elapsed() > Duration::from_secs(15) => {
                    return Err(LoginError::SignOutFailed);
                }
                Some(_) => {}
            },
            (Session::SignedOut, _) => {
                report(LoginStep::FindWindow, progress);
                let since = *signed_out_since.get_or_insert_with(Instant::now);
                match windows::find_client_window() {
                    Some(window) => {
                        let (previous, shown) = window_since.get_or_insert((window, Instant::now()));
                        if *previous != window {
                            *previous = window;
                            *shown = Instant::now();
                        }
                        if since.elapsed() >= SIGNED_OUT_STABLE && shown.elapsed() >= WINDOW_STABLE {
                            break window;
                        }
                    }
                    None => window_since = None,
                }
            }
            (Session::Unknown, _) => {}
        }
        thread::sleep(POLL);
    };

    progress(LoginStep::Focus);
    if !wait_for_shortcut_release() {
        return Err(LoginError::TypingFailed);
    }
    focus_client_with(
        cancel,
        || windows::foreground_is(window),
        || windows::focus(window),
        || thread::sleep(Duration::from_millis(100)),
    )?;
    // Let the login form take focus, then confirm nothing changed meanwhile.
    thread::sleep(Duration::from_millis(1200));
    let still_signed_out = LocalApi::read().is_some_and(|api| api.session() == Session::SignedOut);
    if cancelled() {
        return Err(LoginError::Cancelled);
    }
    if !still_signed_out || !windows::foreground_is(window) {
        return Err(LoginError::TypingFailed);
    }
    progress(LoginStep::Type);
    if !type_credentials(account_id, password, || {
        !cancelled() && windows::foreground_is(window)
    }) {
        check_cancelled(cancel)?;
        return Err(LoginError::TypingFailed);
    }
    progress(LoginStep::Confirm);

    // Launch the game only once this account is confirmed signed in.
    let typed = Instant::now();
    while typed.elapsed() < SIGN_IN_TIMEOUT {
        if cancelled() {
            return Err(LoginError::Cancelled);
        }
        thread::sleep(Duration::from_secs(1));
        if let Some(api) = LocalApi::read() {
            if api.session() == Session::SignedIn && is_this_account(&api) {
                if let Some(game) = game {
                    progress(LoginStep::LaunchGame);
                    start_game(game, account_id, cancel)?;
                }
                progress(LoginStep::Done);
                return Ok(LoginOutcome::SignedIn);
            }
        }
    }
    Ok(LoginOutcome::Typed)
}

/// Activation can be asynchronous or temporarily refused by Windows. Observe
/// the actual foreground window, allowing up to five seconds and requiring
/// 300 ms of stable focus. Do not keep stealing focus once typing has begun.
fn focus_client_with(
    cancel: &AtomicBool,
    mut focused: impl FnMut() -> bool,
    mut activate: impl FnMut(),
    mut wait: impl FnMut(),
) -> Result<(), LoginError> {
    let mut stable = 0;
    for attempt in 0..50 {
        check_cancelled(cancel)?;
        if focused() {
            stable += 1;
            if stable >= 4 {
                return Ok(());
            }
        } else {
            stable = 0;
            if attempt % 5 == 0 {
                activate();
            }
        }
        wait();
    }
    check_cancelled(cancel)?;
    Err(LoginError::TypingFailed)
}

#[cfg(windows)]
mod windows {
    use windows_sys::Win32::Foundation::{CloseHandle, HWND, LPARAM, RECT};
    use windows_sys::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows_sys::Win32::UI::HiDpi::{
        GetDpiForWindow, SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetForegroundWindow, GetWindowRect, GetWindowTextLengthW,
        GetWindowThreadProcessId, IsIconic, IsWindowVisible, SetForegroundWindow, ShowWindow,
        SW_RESTORE,
    };

    /// Executable names of the Riot Client UI across client generations.
    const CLIENT_UI: [&str; 2] = ["riot client.exe", "riotclientux.exe"];

    fn process_name(window: HWND) -> Option<String> {
        let mut pid = 0u32;
        // SAFETY: `window` comes from EnumWindows; `pid` is a valid out pointer.
        unsafe { GetWindowThreadProcessId(window, &mut pid) };
        if pid == 0 {
            return None;
        }
        process_path(pid)?
            .file_name()?
            .to_str()
            .map(str::to_ascii_lowercase)
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
        let ok = unsafe { QueryFullProcessImageNameW(process, 0, buffer.as_mut_ptr(), &mut length) };
        // SAFETY: handle opened above.
        unsafe { CloseHandle(process) };
        if ok == 0 {
            return None;
        }
        let path = String::from_utf16_lossy(&buffer[..length as usize]);
        Some(std::path::PathBuf::from(path))
    }

    unsafe extern "system" fn collect(window: HWND, found: LPARAM) -> i32 {
        // SAFETY: `found` is the &mut Option<HWND> passed to EnumWindows.
        let found = unsafe { &mut *(found as *mut Option<HWND>) };
        // SAFETY: window handles from EnumWindows are valid for these queries.
        let visible = unsafe { IsWindowVisible(window) != 0 && GetWindowTextLengthW(window) > 0 };
        // Skip splash and helper windows: the login UI is a large window
        // (or minimized, in which case focus() restores it).
        let mut rect = RECT { left: 0, top: 0, right: 0, bottom: 0 };
        // SAFETY: rect is a valid out pointer.
        let large = unsafe {
            IsIconic(window) != 0
                || (GetWindowRect(window, &mut rect) != 0
                    && login_window_size(rect, GetDpiForWindow(window)))
        };
        if visible && large && process_name(window).is_some_and(|name| CLIENT_UI.contains(&name.as_str())) {
            *found = Some(window);
            return 0;
        }
        1
    }

    pub fn find_client_window() -> Option<HWND> {
        let mut found: Option<HWND> = None;
        // Query physical bounds regardless of the calling worker's inherited
        // DPI context, then compare in the target window's logical units.
        // SAFETY: this changes only this thread; restore its context afterwards.
        unsafe {
            let previous = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            if previous.is_null() {
                return None;
            }
            EnumWindows(Some(collect), &mut found as *mut _ as LPARAM);
            SetThreadDpiAwarenessContext(previous);
        }
        found
    }

    pub(super) fn login_window_size(rect: RECT, dpi: u32) -> bool {
        dpi != 0
            && (i64::from(rect.right) - i64::from(rect.left)) * 96 >= 600 * i64::from(dpi)
            && (i64::from(rect.bottom) - i64::from(rect.top)) * 96 >= 400 * i64::from(dpi)
    }

    pub fn focus(window: HWND) {
        // An Alt tap may allow activation under foreground-lock rules, but
        // success is established by polling GetForegroundWindow, not this call.
        if !crate::autotype::tap_alt() {
            return;
        }
        // SAFETY: these APIs reject stale handles without dereferencing them.
        unsafe {
            if IsIconic(window) != 0 {
                ShowWindow(window, SW_RESTORE);
            }
            SetForegroundWindow(window);
        }
    }

    pub fn foreground_is(window: HWND) -> bool {
        // SAFETY: read-only query.
        unsafe { GetForegroundWindow() == window }
    }
}

#[cfg(not(windows))]
mod windows {
    pub fn process_path(_: u32) -> Option<std::path::PathBuf> {
        None
    }
    pub type HWND = usize;
    pub fn find_client_window() -> Option<HWND> {
        None
    }
    pub fn focus(_: HWND) {}
    pub fn foreground_is(_: HWND) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn focus_waits_for_delayed_activation_and_retries_refused_requests() {
        let polls = Cell::new(0);
        let requests = Cell::new(0);
        let result = focus_client_with(
            &AtomicBool::new(false),
            || polls.get() >= 8,
            || requests.set(requests.get() + 1),
            || polls.set(polls.get() + 1),
        );
        assert_eq!(result, Ok(()));
        assert_eq!(requests.get(), 2);
        assert_eq!(polls.get(), 11);
    }

    #[test]
    fn focus_must_stabilize_and_gives_up_without_typing() {
        let polls = Cell::new(0);
        let result = focus_client_with(
            &AtomicBool::new(false),
            // Focus is lost after two successful observations.
            || matches!(polls.get(), 1 | 2) || polls.get() >= 6,
            || {},
            || polls.set(polls.get() + 1),
        );
        assert_eq!(result, Ok(()));
        assert_eq!(polls.get(), 9);

        let requests = Cell::new(0);
        assert_eq!(
            focus_client_with(
                &AtomicBool::new(false),
                || false,
                || requests.set(requests.get() + 1),
                || {},
            ),
            Err(LoginError::TypingFailed)
        );
        assert_eq!(requests.get(), 10);
    }

    #[test]
    fn focus_stops_when_cancelled() {
        let cancel = AtomicBool::new(true);
        assert_eq!(
            focus_client_with(
                &cancel,
                || panic!("must not query after cancellation"),
                || panic!("must not activate after cancellation"),
                || panic!("must not wait after cancellation"),
            ),
            Err(LoginError::Cancelled)
        );
        cancel.store(false, Ordering::SeqCst);
        let requests = Cell::new(0);
        assert_eq!(
            focus_client_with(
                &cancel,
                || false,
                || requests.set(requests.get() + 1),
                || cancel.store(true, Ordering::SeqCst),
            ),
            Err(LoginError::Cancelled)
        );
        assert_eq!(requests.get(), 1);
    }

    #[test]
    #[cfg(windows)]
    fn window_size_is_independent_of_monitor_origin_and_scaling() {
        use windows_sys::Win32::Foundation::RECT;
        for (left, top) in [(0, 0), (-3840, -2160), (3840, 2160)] {
            for dpi in [96, 120, 144, 192, 288] {
                let rect = RECT {
                    left,
                    top,
                    right: left + 600 * dpi as i32 / 96,
                    bottom: top + 400 * dpi as i32 / 96,
                };
                assert!(windows::login_window_size(rect, dpi));
                assert!(!windows::login_window_size(
                    RECT {
                        right: rect.right - 1,
                        ..rect
                    },
                    dpi
                ));
                assert!(!windows::login_window_size(
                    RECT {
                        bottom: rect.bottom - 1,
                        ..rect
                    },
                    dpi
                ));
                assert!(!windows::login_window_size(rect, 0));
            }
        }
    }

    #[test]
    #[cfg(windows)]
    fn window_discovery_restores_the_callers_dpi_context() {
        use windows_sys::Win32::UI::HiDpi::{
            AreDpiAwarenessContextsEqual, GetThreadDpiAwarenessContext, SetThreadDpiAwarenessContext,
            DPI_AWARENESS_CONTEXT_SYSTEM_AWARE, DPI_AWARENESS_CONTEXT_UNAWARE,
        };
        // Read-only window discovery; no focus changes or injected input.
        unsafe {
            for context in [
                DPI_AWARENESS_CONTEXT_UNAWARE,
                DPI_AWARENESS_CONTEXT_SYSTEM_AWARE,
            ] {
                let original = SetThreadDpiAwarenessContext(context);
                assert!(!original.is_null());
                let _ = windows::find_client_window();
                let restored = AreDpiAwarenessContextsEqual(GetThreadDpiAwarenessContext(), context);
                SetThreadDpiAwarenessContext(original);
                assert_ne!(restored, 0);
            }
        }
    }

    #[test]
    #[cfg(windows)]
    fn process_discovery_locates_the_executable_for_a_running_pid() {
        let processes = running_processes();
        let current = processes.iter()
            .find(|process| process.pid == std::process::id()).unwrap();
        let executable = std::env::current_exe().unwrap();
        assert_eq!(current.name,
            executable.file_name().unwrap().to_str().unwrap().to_ascii_lowercase());
        assert_eq!(windows::process_path(current.pid), Some(executable));
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
                    Ok(true)
                },
                || {},
            );
            assert_eq!(result, expected);
            assert_eq!(requests.get(), 1);
            assert!(polls.get() > 1);
        }

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
            LoginStep::LaunchGame.record();
            record_login_result(&Err(LoginError::AccountMismatch));
            record_login_result(&Err(LoginError::GameLaunchFailed));
            record_login_result(&Err(LoginError::SignOutFailed));
            record_login_result(&Err(LoginError::Cancelled));
            record_login_result(&Ok(LoginOutcome::AlreadySignedIn));
        });
        for entry in [
            "INFO event=login_progress reason=launch_game",
            "ERROR event=login_failed reason=account_mismatch",
            "ERROR event=login_failed reason=game_launch_failed",
            "ERROR event=login_failed reason=sign_out_failed",
            "INFO event=login_cancelled reason=cancelled",
            "INFO event=login_completed reason=already_signed_in",
        ] {
            assert!(log.contains(entry));
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
            let body = r#"{"functions":{"DeleteRsoAuthV1Session":{}}}"#;
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
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
