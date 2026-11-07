//! Automatic updates: while they are on, check at launch and every few hours,
//! install the release executable in the background, and restart into
//! it when the UI reports the app is idle (or right away from the launch update
//! screen). When they are off, only Settings › Check now contacts GitHub.
//!
//! The swap happens as soon as the download is verified, so declining the
//! restart, closing the app or a failed restart all still start the new
//! version next time.

use crate::refresh::{Settings, Shared};
use crate::views::{fail, fail_with, CommandResult};
use leagueaccounts::logging::{self, Event, Reason};
use leagueaccounts::self_update::{self, Paths};
use leagueaccounts::updates::{self, Download, Release, UpdateError};
use serde::Serialize;
use std::ffi::OsString;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use tauri::{plugin::TauriPlugin, AppHandle, Emitter, Manager, RunEvent, Wry};

const CHECK_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
/// After a failed check (offline at boot, for example), try again sooner.
const RETRY_INTERVAL: Duration = Duration::from_secs(15 * 60);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Phase {
    /// Not checked yet, or the last check failed.
    #[default]
    Idle,
    Checking,
    UpToDate,
    /// A newer release that is not installed automatically: show the banner.
    Available,
    Downloading,
    /// Installed; restarting (or the next launch) runs it.
    Ready,
    Restarting,
    /// The automatic install failed: show the banner for a manual download.
    Failed,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStatus {
    phase: Phase,
    current: &'static str,
    /// The latest release when known, newer or not.
    release: Option<Release>,
    received: u64,
    total: u64,
    /// The check the app starts with, which may hold the loading screen.
    launch: bool,
    /// Increases with every change, so the UI can ignore an older snapshot.
    seq: u64,
}

pub struct Updater {
    status: Mutex<UpdateStatus>,
    /// One check or install at a time, from the scheduler or Settings.
    busy: AtomicBool,
    paths: Option<Paths>,
    /// A release whose executable reported an older version: it is only
    /// offered for download from now on (see `Settings::mislabeled_release`).
    mislabeled: Option<String>,
    /// Arguments for the new version, set when the app exits to restart.
    restart: Mutex<Option<Vec<OsString>>>,
}

impl Updater {
    pub fn new(settings: &Settings) -> Self {
        Self {
            status: Mutex::new(UpdateStatus {
                phase: Phase::Idle,
                current: env!("CARGO_PKG_VERSION"),
                release: None,
                received: 0,
                total: 0,
                // No launch check, and so no wait on the loading screen, when off.
                launch: settings.auto_update,
                seq: 0,
            }),
            busy: AtomicBool::new(false),
            paths: Paths::current(),
            mislabeled: settings.mislabeled_release.clone(),
            restart: Mutex::new(None),
        }
    }

    pub fn status(&self) -> UpdateStatus {
        self.status.lock().unwrap_or_else(|error| error.into_inner()).clone()
    }

    fn set(&self, app: &AppHandle, change: impl FnOnce(&mut UpdateStatus)) {
        let snapshot = {
            let mut status = self.status.lock().unwrap_or_else(|error| error.into_inner());
            change(&mut status);
            status.seq += 1;
            status.clone()
        };
        let _ = app.emit("update-status", snapshot);
    }

    fn installed(&self) -> bool {
        matches!(self.status().phase, Phase::Ready | Phase::Restarting)
    }
}

/// Held from the check until its install finishes, possibly on another thread.
struct Busy(Arc<Shared>);

impl Busy {
    fn take(shared: &Arc<Shared>) -> Option<Self> {
        (!shared.updater.busy.swap(true, Ordering::SeqCst)).then(|| Self(Arc::clone(shared)))
    }
}

impl Drop for Busy {
    fn drop(&mut self) {
        self.0.updater.busy.store(false, Ordering::SeqCst);
    }
}

struct Install {
    busy: Busy,
    download: Download,
    paths: Paths,
}

/// Look for a newer release; returns the install to run when one should start.
fn check(app: &AppHandle, shared: &Arc<Shared>, launch: bool) -> Result<Option<Install>, UpdateError> {
    let Some(busy) = Busy::take(shared) else {
        return Ok(None);
    };
    let updater = &shared.updater;
    if updater.installed() {
        return Ok(None);
    }
    updater.set(app, |status| {
        status.phase = Phase::Checking;
        status.launch = launch;
    });
    let latest = match updates::latest_release() {
        Ok(latest) => latest,
        Err(error) => {
            logging::record(Event::UpdateCheckFailed, error.reason());
            updater.set(app, |status| status.phase = Phase::Idle);
            return Err(error);
        }
    };
    let newer = latest
        .as_ref()
        .is_some_and(|release| updates::is_newer(&release.version, env!("CARGO_PKG_VERSION")));
    let automatic = shared.settings.lock().map(|settings| settings.auto_update).unwrap_or(false);
    let install = match (&latest, &updater.paths) {
        (Some(release), Some(paths))
            if newer
                && automatic
                && updates::can_install()
                && updater.mislabeled.as_ref() != Some(&release.version) =>
        {
            release.executable.clone().map(|download| Install {
                busy,
                download,
                paths: paths.clone(),
            })
        }
        _ => None,
    };
    updater.set(app, |status| {
        status.phase = match (&install, newer) {
            (Some(_), _) => Phase::Downloading,
            (None, true) => Phase::Available,
            (None, false) => Phase::UpToDate,
        };
        status.received = 0;
        status.total = install.as_ref().map_or(0, |install| install.download.size);
        status.release = latest;
    });
    Ok(install)
}

fn install(app: &AppHandle, job: Install) {
    let updater = &job.busy.0.updater;
    match download_and_swap(app, &job) {
        Ok(()) => {
            logging::record(Event::UpdateInstalled, Reason::None);
            updater.set(app, |status| status.phase = Phase::Ready);
        }
        Err((event, error)) => {
            // Keep the download if a failed swap could not restore the executable.
            if job.paths.exe.exists() {
                let _ = std::fs::remove_file(&job.paths.staged);
            }
            logging::record(event, error.reason());
            updater.set(app, |status| status.phase = Phase::Failed);
        }
    }
}

fn download_and_swap(app: &AppHandle, job: &Install) -> Result<(), (Event, UpdateError)> {
    let failed = |error: UpdateError| (Event::UpdateFailed, error);
    let updater = &job.busy.0.updater;
    let total = job.download.size;
    let mut file = self_update::create_staged(&job.paths).map_err(|error| failed(error.into()))?;
    let mut reported = 0;
    updates::download(&job.download, &mut file, |received| {
        // About a hundred progress events per download.
        if received == total || received - reported >= total / 100 {
            reported = received;
            updater.set(app, |status| status.received = received);
        }
    })
    .map_err(failed)?;
    drop(file);
    logging::record(Event::UpdateDownloaded, Reason::None);
    updates::verify_file(&job.paths.staged, &job.download.sha256)
        .map_err(|error| (Event::UpdateVerificationFailed, error))?;
    let target = updater.status().release.ok_or_else(|| failed(UpdateError::from(std::io::Error::other("missing update target"))))?;
    self_update::swap(&job.paths, &target.version).map_err(|error| failed(error.into()))
}

/// While automatic updates are on, check right away at launch and then every
/// few hours, until an update is installed. Turning them on in Settings starts
/// a check within a minute.
pub fn spawn(app: AppHandle, shared: Arc<Shared>) {
    thread::spawn(move || {
        let mut launch = true;
        let mut next_check = Instant::now();
        loop {
            let automatic = shared.settings.lock().map(|settings| settings.auto_update).unwrap_or(false);
            if automatic && Instant::now() >= next_check {
                let wait = match check(&app, &shared, launch) {
                    Ok(Some(job)) => {
                        install(&app, job);
                        CHECK_INTERVAL
                    }
                    Ok(None) => CHECK_INTERVAL,
                    Err(_) => RETRY_INTERVAL,
                };
                next_check = Instant::now() + wait;
            }
            if shared.updater.installed() {
                return;
            }
            launch = false;
            thread::sleep(Duration::from_secs(60));
        }
    });
}

/// Settings › Check now. An install continues in the background.
pub fn check_now(app: AppHandle, shared: Arc<Shared>) -> CommandResult<UpdateStatus> {
    match check(&app, &shared, false) {
        Ok(Some(job)) => {
            let app = app.clone();
            thread::spawn(move || install(&app, job));
        }
        Ok(None) => {}
        Err(error) => return Err(fail_with("update_failed", error)),
    }
    Ok(shared.updater.status())
}

/// No login, rank refresh or match in progress (the UI checks its own dialogs).
pub fn idle(shared: &Shared) -> bool {
    !shared.login.has_refreshes() && !shared.login.is_running() && !leagueaccounts::riot_client::in_game()
}

/// Exit and start the installed version, unless a login or refresh is running.
pub fn restart(app: &AppHandle, shared: &Shared) -> CommandResult<()> {
    let updater = &shared.updater;
    if updater.status().phase != Phase::Ready {
        return Err(fail("update_not_ready"));
    }
    if shared.login.has_refreshes() {
        return Err(fail("refresh_running"));
    }
    if !shared.login.close_if_idle() {
        return Err(fail("login_busy"));
    }
    // Never bring the window in front of another app, a game in particular.
    let minimized = app.get_webview_window("main").is_some_and(|window| {
        window.is_minimized().unwrap_or(false) || !window.is_focused().unwrap_or(true)
    });
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    *updater.restart.lock().unwrap_or_else(|error| error.into_inner()) =
        Some(self_update::restart_args(&args, std::process::id(), minimized));
    updater.set(app, |status| status.phase = Phase::Restarting);
    app.exit(0);
    Ok(())
}

/// Start the new version once the event loop exits, after login cleanup.
pub fn plugin(shared: Arc<Shared>) -> TauriPlugin<Wry> {
    tauri::plugin::Builder::new("self-update")
        .on_event(move |_app, event| {
            if !matches!(event, RunEvent::Exit) {
                return;
            }
            let args = shared.updater.restart.lock().ok().and_then(|mut args| args.take());
            let (Some(args), Some(paths)) = (args, &shared.updater.paths) else {
                return;
            };
            // The next launch runs the new version anyway.
            if let Err(error) = std::process::Command::new(&paths.exe).args(args).spawn() {
                logging::record(Event::UpdateFailed, Reason::from_error(&error));
            }
        })
        .build()
}

/// Before anything else at startup: after an update, wait for the previous
/// version to exit, then remove what it left behind.
pub fn finish_previous_update() {
    let args: Vec<OsString> = std::env::args_os().collect();
    let previous = self_update::previous_process(&args);
    if let Some(pid) = previous {
        if !self_update::wait_for_exit(pid, Duration::from_secs(30)) {
            logging::record(Event::UpdateFailed, Reason::Timeout);
        }
    }
    if let Some(paths) = Paths::current() {
        self_update::clean_up(&paths, previous.is_some());
    }
    if previous.is_some() {
        logging::record(Event::UpdateRestarted, Reason::None);
    }
    if mislabeled_release().is_some() {
        logging::record(Event::UpdateFailed, Reason::VersionMismatch);
    }
}

/// The release this process was installed for, if this build reports an older
/// version: installing it again would restart into the same build forever.
pub fn mislabeled_release() -> Option<String> {
    self_update::installed_target(&Paths::current()?)
        .filter(|release| updates::is_newer(release, env!("CARGO_PKG_VERSION")))
}

pub fn start_minimized_after_restart() -> bool {
    std::env::args_os().any(|arg| arg == self_update::MINIMIZED_FLAG)
}
