//! Plain-text support logs with an allowlist, not string redaction.
//!
//! Only `Event` and `Reason` can cross the logging boundary. Never add a
//! free-form message, account field, error Display/Debug output, path, URL,
//! panic payload, thread name, or backtrace here. Dependency loggers are
//! deliberately not connected to this file sink.

use std::error::Error;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_FILE_BYTES: u64 = 1024 * 1024;
const MAX_LOG_FILES: usize = 20;
static LOGGER: OnceLock<Mutex<FileLogger>> = OnceLock::new();

/// Fixed diagnostic events. No variant may contain user-controlled data.
#[derive(Clone, Copy)]
pub enum Event {
    SessionStarted,
    SessionEnded,
    AppRunFailed,
    DataDirectoryFailed,
    AccountsLoadFailed,
    AccountsSaveFailed,
    ImportFailed,
    ExportFailed,
    CredentialReadFailed,
    CredentialWriteFailed,
    CredentialDeleteFailed,
    RankClientFailed,
    RankFetchFailed,
    TftFetchFailed,
    SettingsSaveFailed,
    AutoRefreshStarted,
    RankRefreshStarted,
    RankRefreshCompleted,
    RankWorkerPanicked,
    ShortcutInstallFailed,
    ClipboardFailed,
    LoginStarted,
    LoginProgress,
    LoginWaiting,
    LoginCompleted,
    LoginFailed,
    LoginCancelled,
    LogFolderOpenFailed,
    Panic,
}

impl Event {
    fn code(self) -> &'static str {
        match self {
            Self::SessionStarted => "session_started",
            Self::SessionEnded => "session_ended",
            Self::AppRunFailed => "app_run_failed",
            Self::DataDirectoryFailed => "data_directory_failed",
            Self::AccountsLoadFailed => "accounts_load_failed",
            Self::AccountsSaveFailed => "accounts_save_failed",
            Self::ImportFailed => "import_failed",
            Self::ExportFailed => "export_failed",
            Self::CredentialReadFailed => "credential_read_failed",
            Self::CredentialWriteFailed => "credential_write_failed",
            Self::CredentialDeleteFailed => "credential_delete_failed",
            Self::RankClientFailed => "rank_client_failed",
            Self::RankFetchFailed => "rank_fetch_failed",
            Self::TftFetchFailed => "tft_fetch_failed",
            Self::SettingsSaveFailed => "settings_save_failed",
            Self::AutoRefreshStarted => "auto_refresh_started",
            Self::RankRefreshStarted => "rank_refresh_started",
            Self::RankRefreshCompleted => "rank_refresh_completed",
            Self::RankWorkerPanicked => "rank_worker_panicked",
            Self::ShortcutInstallFailed => "shortcut_install_failed",
            Self::ClipboardFailed => "clipboard_failed",
            Self::LoginStarted => "login_started",
            Self::LoginProgress => "login_progress",
            Self::LoginWaiting => "login_waiting",
            Self::LoginCompleted => "login_completed",
            Self::LoginFailed => "login_failed",
            Self::LoginCancelled => "login_cancelled",
            Self::LogFolderOpenFailed => "log_folder_open_failed",
            Self::Panic => "panic",
        }
    }

    fn level(self) -> &'static str {
        match self {
            Self::SessionStarted
            | Self::SessionEnded
            | Self::LoginStarted
            | Self::LoginProgress
            | Self::LoginWaiting
            | Self::LoginCompleted
            | Self::LoginCancelled
            | Self::RankRefreshStarted
            | Self::RankRefreshCompleted
            | Self::AutoRefreshStarted => "INFO",
            _ => "ERROR",
        }
    }
}

/// Safe categories, deliberately excluding raw messages and arbitrary codes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    ConnectRenderer,
    RendererFailed,
    AuthRejected,
    CaptchaRejected,
    RiotClient,
    PreviousWindow,
    CloseLeague,
    OpenClient,
    WaitAuth,
    AuthUnavailable,
    AuthStarting,
    AuthInteraction,
    AuthIdentityPending,
    LoginFormReady,
    GameIdentityPending,
    GameProcessPending,
    LaunchRejected,
    SignOut,
    FindWindow,
    Focus,
    Type,
    Confirm,
    LaunchGame,
    Done,
    SignedIn,
    Typed,
    AlreadySignedIn,
    ClientMissing,
    LaunchFailed,
    SignOutFailed,
    TypingFailed,
    Cancelled,
    LeagueRunning,
    CloseFailed,
    GameLaunchFailed,
    AccountMismatch,
    WorkerPanicked,
    None,
    PermissionDenied,
    NotFound,
    AlreadyExists,
    InvalidData,
    Storage,
    CredentialStore,
    Timeout,
    Connection,
    Request,
    HttpUnauthorized,
    HttpForbidden,
    HttpNotFound,
    HttpRateLimited,
    HttpServer,
    HttpOther,
    ResponseBody,
    ProfileMissing,
    Other,
}

impl Reason {
    fn code(self) -> &'static str {
        match self {
            Self::ConnectRenderer => "connect_renderer",
            Self::RendererFailed => "renderer_failed",
            Self::AuthRejected => "auth_rejected",
            Self::CaptchaRejected => "captcha_rejected",
            Self::RiotClient => "riot_client",
            Self::PreviousWindow => "previous_window",
            Self::CloseLeague => "close_league",
            Self::OpenClient => "open_client",
            Self::WaitAuth => "wait_auth",
            Self::AuthUnavailable => "auth_unavailable",
            Self::AuthStarting => "auth_starting",
            Self::AuthInteraction => "auth_interaction",
            Self::AuthIdentityPending => "auth_identity_pending",
            Self::LoginFormReady => "login_form_ready",
            Self::GameIdentityPending => "game_identity_pending",
            Self::GameProcessPending => "game_process_pending",
            Self::LaunchRejected => "launch_rejected",
            Self::SignOut => "sign_out",
            Self::FindWindow => "find_window",
            Self::Focus => "focus",
            Self::Type => "type",
            Self::Confirm => "confirm",
            Self::LaunchGame => "launch_game",
            Self::Done => "done",
            Self::SignedIn => "signed_in",
            Self::Typed => "typed",
            Self::AlreadySignedIn => "already_signed_in",
            Self::ClientMissing => "client_missing",
            Self::LaunchFailed => "launch_failed",
            Self::SignOutFailed => "sign_out_failed",
            Self::TypingFailed => "typing_failed",
            Self::Cancelled => "cancelled",
            Self::LeagueRunning => "league_running",
            Self::CloseFailed => "close_failed",
            Self::GameLaunchFailed => "game_launch_failed",
            Self::AccountMismatch => "account_mismatch",
            Self::WorkerPanicked => "worker_panicked",
            Self::None => "none",
            Self::PermissionDenied => "permission_denied",
            Self::NotFound => "not_found",
            Self::AlreadyExists => "already_exists",
            Self::InvalidData => "invalid_data",
            Self::Storage => "storage",
            Self::CredentialStore => "credential_store",
            Self::Timeout => "timeout",
            Self::Connection => "connection",
            Self::Request => "request",
            Self::HttpUnauthorized => "http_unauthorized",
            Self::HttpForbidden => "http_forbidden",
            Self::HttpNotFound => "http_not_found",
            Self::HttpRateLimited => "http_rate_limited",
            Self::HttpServer => "http_server",
            Self::HttpOther => "http_other",
            Self::ResponseBody => "response_body",
            Self::ProfileMissing => "profile_missing",
            Self::Other => "other",
        }
    }

    /// Inspect types/categories only. Never format an error or its source.
    pub fn from_error(error: &(dyn Error + 'static)) -> Self {
        if let Some(error) = error.downcast_ref::<io::Error>() {
            match error.kind() {
                io::ErrorKind::PermissionDenied => Self::PermissionDenied,
                io::ErrorKind::NotFound => Self::NotFound,
                io::ErrorKind::AlreadyExists => Self::AlreadyExists,
                io::ErrorKind::InvalidData => Self::InvalidData,
                _ => Self::Storage,
            }
        } else if error.is::<serde_json::Error>() {
            Self::InvalidData
        } else {
            Self::Other
        }
    }

    pub fn from_request(error: &reqwest::Error) -> Self {
        if error.is_timeout() {
            Self::Timeout
        } else if error.is_connect() {
            Self::Connection
        } else if let Some(status) = error.status() {
            match status.as_u16() {
                401 => Self::HttpUnauthorized,
                403 => Self::HttpForbidden,
                404 => Self::HttpNotFound,
                429 => Self::HttpRateLimited,
                500..=599 => Self::HttpServer,
                _ => Self::HttpOther,
            }
        } else {
            Self::Request
        }
    }
}

/// Initialize before loading accounts or constructing the UI.
pub fn init() -> io::Result<()> {
    let directory = crate::utils::app_data_dir()?.join("logs");
    let logger = FileLogger::new(directory, MAX_FILE_BYTES)?;
    LOGGER
        .set(Mutex::new(logger))
        .map_err(|_| io::Error::from(io::ErrorKind::AlreadyExists))?;
    record(Event::SessionStarted, Reason::None);
    Ok(())
}

/// Do not chain the default hook: panic payloads can contain credentials.
pub fn install_panic_hook() {
    std::panic::set_hook(Box::new(|_info| record(Event::Panic, Reason::None)));
}

/// The only log-writing API accepts closed enums, never strings.
///
/// ```compile_fail
/// leagueaccounts::logging::record("user-supplied message", "raw error");
/// ```
pub fn record(event: Event, reason: Reason) {
    #[cfg(test)]
    if capture_for_test(event, reason) {
        return;
    }
    if let Some(logger) = LOGGER.get() {
        if let Ok(mut logger) = logger.lock() {
            logger.healthy = logger.write(event, reason).is_ok();
        }
    }
}

pub fn is_available() -> bool {
    LOGGER
        .get()
        .and_then(|logger| logger.lock().ok())
        .is_some_and(|logger| logger.healthy)
}

pub fn directory() -> Option<PathBuf> {
    LOGGER
        .get()
        .and_then(|logger| logger.lock().ok())
        .map(|logger| logger.directory.clone())
}

fn unix_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn line(event: Event, reason: Reason) -> String {
    format!(
        "unix_ms={} {} event={} reason={} app_version={} os={} arch={}\n",
        unix_ms(),
        event.level(),
        event.code(),
        reason.code(),
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
    )
}

struct FileLogger {
    directory: PathBuf,
    session: String,
    segment: u64,
    file: File,
    bytes: u64,
    max_bytes: u64,
    healthy: bool,
}

impl FileLogger {
    fn new(directory: PathBuf, max_bytes: u64) -> io::Result<Self> {
        fs::create_dir_all(&directory)?;
        let session = format!("leagueaccounts-{:020}-{}", unix_ms(), std::process::id());
        let mut segment = 0;
        let file = Self::open_segment(&directory, &session, &mut segment)?;
        prune_logs(&directory);
        Ok(Self {
            directory,
            session,
            segment,
            file,
            bytes: 0,
            max_bytes,
            healthy: true,
        })
    }

    fn open_segment(directory: &Path, session: &str, segment: &mut u64) -> io::Result<File> {
        loop {
            let path = directory.join(format!("{session}-{:06}.log", *segment));
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(windows)]
            {
                use std::os::windows::fs::OpenOptionsExt;
                // FILE_SHARE_READ: users can inspect the log, but retention
                // cannot delete a file still being written by another app.
                options.share_mode(1);
            }
            match options.open(path) {
                Ok(file) => return Ok(file),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => *segment += 1,
                Err(error) => return Err(error),
            }
        }
    }

    fn write(&mut self, event: Event, reason: Reason) -> io::Result<()> {
        let line = line(event, reason);
        if self.bytes > 0 && self.bytes + line.len() as u64 > self.max_bytes {
            self.segment += 1;
            self.file = Self::open_segment(&self.directory, &self.session, &mut self.segment)?;
            self.bytes = 0;
            prune_logs(&self.directory);
        }
        self.file.write_all(line.as_bytes())?;
        self.file.flush()?;
        self.bytes += line.len() as u64;
        Ok(())
    }
}

// Only remove our numeric log filenames; never touch unrelated files.
fn is_log_filename(name: &str) -> bool {
    name.strip_prefix("leagueaccounts-")
        .and_then(|name| name.strip_suffix(".log"))
        .is_some_and(|name| {
            let parts: Vec<_> = name.split('-').collect();
            parts.len() == 3
                && parts
                    .iter()
                    .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
        })
}

fn prune_logs(directory: &Path) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    let mut logs: Vec<_> = entries
        .flatten()
        .filter(|entry| {
            entry.file_type().is_ok_and(|kind| kind.is_file())
                && entry.file_name().to_str().is_some_and(is_log_filename)
        })
        .collect();
    logs.sort_by_key(|entry| entry.file_name());
    let excess = logs.len().saturating_sub(MAX_LOG_FILES);
    for entry in logs.into_iter().take(excess) {
        // Active files from other app instances may be locked on Windows.
        let _ = fs::remove_file(entry.path());
    }
}

#[cfg(test)]
thread_local! {
    static TEST_LOG: std::cell::RefCell<Option<Vec<String>>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn capture_for_test(event: Event, reason: Reason) -> bool {
    TEST_LOG.with(|log| {
        if let Some(lines) = log.borrow_mut().as_mut() {
            lines.push(line(event, reason));
            true
        } else {
            false
        }
    })
}

#[cfg(test)]
pub(crate) fn capture(action: impl FnOnce()) -> String {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            TEST_LOG.with(|log| *log.borrow_mut() = None);
        }
    }
    let _reset = Reset;
    TEST_LOG.with(|log| *log.borrow_mut() = Some(Vec::new()));
    action();
    TEST_LOG.with(|log| log.borrow_mut().take().unwrap().concat())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_with_secrets_are_classified_without_formatting() {
        #[derive(Debug)]
        struct HostileError;
        impl std::fmt::Display for HostileError {
            fn fmt(&self, _: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                panic!("error messages must never be inspected");
            }
        }
        impl Error for HostileError {}
        let error = io::Error::new(io::ErrorKind::PermissionDenied, HostileError);
        let directory = tempfile::tempdir().unwrap();
        let mut logger = FileLogger::new(directory.path().to_owned(), MAX_FILE_BYTES).unwrap();
        logger
            .write(Event::ImportFailed, Reason::from_error(&error))
            .unwrap();
        let text = fs::read_to_string(
            fs::read_dir(directory.path())
                .unwrap()
                .next()
                .unwrap()
                .unwrap()
                .path(),
        )
        .unwrap();
        assert!(text.contains("event=import_failed reason=permission_denied"));
        assert_eq!(Reason::from_error(&HostileError), Reason::Other);
    }

    #[test]
    fn logs_rotate_retain_recent_errors_and_preserve_unrelated_files() {
        let directory = tempfile::tempdir().unwrap();
        let unrelated = directory.path().join("notes.log");
        fs::write(&unrelated, "keep this").unwrap();
        let mut logger = FileLogger::new(directory.path().to_owned(), 256).unwrap();
        for _ in 0..60 {
            logger
                .write(Event::RankFetchFailed, Reason::Timeout)
                .unwrap();
        }
        logger.write(Event::Panic, Reason::None).unwrap();
        let mut logs: Vec<_> = fs::read_dir(directory.path())
            .unwrap()
            .flatten()
            .filter(|entry| is_log_filename(entry.file_name().to_str().unwrap()))
            .collect();
        assert_eq!(logs.len(), MAX_LOG_FILES);
        assert!(logs
            .iter()
            .all(|entry| entry.metadata().unwrap().len() <= 256));
        logs.sort_by_key(|entry| entry.file_name());
        assert!(fs::read_to_string(logs.last().unwrap().path())
            .unwrap()
            .contains("event=panic"));
        assert_eq!(fs::read_to_string(unrelated).unwrap(), "keep this");
    }

    #[test]
    fn concurrent_workers_write_complete_plain_text_lines() {
        let directory = tempfile::tempdir().unwrap();
        let logger = std::sync::Arc::new(Mutex::new(
            FileLogger::new(directory.path().to_owned(), MAX_FILE_BYTES).unwrap(),
        ));
        let workers: Vec<_> = (0..4)
            .map(|_| {
                let logger = logger.clone();
                std::thread::spawn(move || {
                    for _ in 0..50 {
                        logger
                            .lock()
                            .unwrap()
                            .write(Event::RankFetchFailed, Reason::Timeout)
                            .unwrap();
                    }
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
        let path = fs::read_dir(directory.path())
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let text = fs::read_to_string(path).unwrap();
        assert_eq!(text.lines().count(), 200);
        assert!(
            text.lines()
                .all(|line| line
                    .contains("ERROR event=rank_fetch_failed reason=timeout app_version="))
        );
    }
}
