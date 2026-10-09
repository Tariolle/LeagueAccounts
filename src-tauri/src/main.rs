#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! Tauri shell: exposes the account manager to the web interface.
//!
//! Passwords never cross into the web view. The UI only receives account
//! metadata; copy/login commands read the password on the Rust side.

mod login_control;
mod refresh;
mod views;

use leagueaccounts::account_manager::KEYRING_SERVICE;
use leagueaccounts::autotype::auto_type_credentials;
use leagueaccounts::credentials;
use leagueaccounts::logging::{self, Event, Reason};
use leagueaccounts::models::{Account, AccountKey, RankInfo};
use leagueaccounts::rank_fetcher::{RankFetcher, RankProvider};
use leagueaccounts::riot_client::{self, Game, LoginError, LoginOutcome, LoginStep};
use leagueaccounts::utils::{app_data_dir, parse_account_line, region_from_display, settings_file, REGION_MAP};
use leagueaccounts::AccountManager;
use leagueaccounts::updates::{self, Release};
use login_control::LoginControl;
use refresh::{
    schedule, spawn_scheduler, spawn_update_checker, start_refresh, LoginMethod, RefreshKind, Schedule, Settings, Shared,
    MAX_INTERVAL_MINUTES, MIN_INTERVAL_MINUTES,
};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};
use views::{account_view, fail, fail_with, AccountView, AppError, CommandResult, Key};

/// Copied passwords are wiped from the clipboard after this delay.
const PASSWORD_CLIPBOARD_TTL: Duration = Duration::from_secs(30);
/// Command-line flag the autostart entry passes to the app.
const AUTOSTART_FLAG: &str = "--autostart";

type AppState<'a> = State<'a, Arc<Shared>>;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SettingsView {
    auto_refresh: bool,
    interval_minutes: u32,
    start_minimized: bool,
    launch_at_startup: bool,
    login_method: LoginMethod,
    launch_game: bool,
    check_updates: bool,
    riot_client_found: bool,
    tft_installed: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SettingsInput {
    auto_refresh: bool,
    interval_minutes: u32,
    start_minimized: bool,
    launch_at_startup: bool,
    login_method: LoginMethod,
    launch_game: bool,
    check_updates: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Bootstrap {
    accounts: Vec<AccountView>,
    regions: Vec<String>,
    version: &'static str,
    logging_available: bool,
    load_error: Option<AppError>,
    settings: SettingsView,
    schedule: Schedule,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NewAccount {
    account_id: String,
    #[serde(default)]
    name: String,
    region: String,
    password: String,
    description: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BatchResult {
    added: usize,
    skipped: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BulkAddResult {
    added: usize,
    skipped_lines: Vec<usize>,
}

fn manager(state: &Shared) -> CommandResult<MutexGuard<'_, AccountManager>> {
    state.manager.lock().map_err(|_| fail("storage_locked"))
}

fn views(state: &Shared) -> CommandResult<Vec<AccountView>> {
    let manager = manager(state)?;
    Ok(manager
        .accounts
        .iter()
        .map(account_view)
        .collect())
}

fn password_for(account: &Account) -> String {
    if !account.password.is_empty() {
        return account.password.clone();
    }
    credentials::get_password(
        KEYRING_SERVICE,
        &format!("{}:{}", account.region, account.account_id),
    )
    .ok()
    .flatten()
    .unwrap_or_default()
}

fn find_account(state: &Shared, key: Key) -> CommandResult<Account> {
    manager(state)?
        .account(&AccountKey::from(key))
        .cloned()
        .ok_or_else(|| fail("not_found"))
}

fn settings_view(app: &AppHandle, state: &Shared) -> SettingsView {
    let settings = state.settings.lock().map(|s| s.clone()).unwrap_or_default();
    SettingsView {
        auto_refresh: settings.auto_refresh,
        interval_minutes: settings.interval_minutes,
        start_minimized: settings.start_minimized,
        launch_at_startup: app.autolaunch().is_enabled().unwrap_or(false),
        login_method: settings.login_method,
        launch_game: settings.launch_game,
        check_updates: settings.check_updates,
        riot_client_found: riot_client::client_path().is_some(),
        tft_installed: riot_client::game_installed(Game::Tft),
    }
}

#[tauri::command]
fn bootstrap(app: AppHandle, state: AppState<'_>) -> CommandResult<Bootstrap> {
    Ok(Bootstrap {
        accounts: views(&state)?,
        regions: REGION_MAP
            .iter()
            .map(|(label, _)| (*label).to_owned())
            .collect(),
        version: env!("CARGO_PKG_VERSION"),
        logging_available: logging::is_available(),
        load_error: state.load_error.clone(),
        settings: settings_view(&app, &state),
        schedule: schedule(&state),
    })
}

#[tauri::command]
fn list_accounts(state: AppState<'_>) -> CommandResult<Vec<AccountView>> {
    views(&state)
}

#[tauri::command]
fn add_account(app: AppHandle, state: AppState<'_>, input: NewAccount) -> CommandResult<AccountView> {
    let account_id = input.account_id.trim().to_owned();
    let name = input.name.trim().to_owned();
    let password = input.password.trim().to_owned();
    if account_id.is_empty() || password.is_empty() {
        return Err(fail("required_fields"));
    }
    let region_label = input.region.trim().to_owned();
    let region = region_from_display(&region_label).ok_or_else(|| fail("invalid_region"))?;
    let account = {
        let mut manager = manager(&state)?;
        if manager.accounts.iter().any(|account| {
            account.account_id.eq_ignore_ascii_case(&account_id) && account.region == region
        }) {
            return Err(fail("duplicate"));
        }
        let account = Account {
            account_id,
            name,
            region: region.to_owned(),
            region_display: region_label,
            password,
            description: input.description.trim().to_owned(),
            tier: "Unranked".to_owned(),
            reached_last_season: "N/A".to_owned(),
            finished_last_season: "N/A".to_owned(),
            ..Account::default()
        };
        let result = manager
            .add_accounts(vec![account])
            .map_err(|error| fail_with("save_failed", error))?;
        result.added.into_iter().next().ok_or_else(|| fail("password_save"))?
    };
    let view = account_view(&account);
    start_refresh(app, Arc::clone(&state), vec![account], RefreshKind::Partial);
    Ok(view)
}

#[tauri::command]
fn multi_add(app: AppHandle, state: AppState<'_>, text: String, region: String) -> CommandResult<BulkAddResult> {
    let region_label = region.trim().to_owned();
    let region = region_from_display(&region_label).ok_or_else(|| fail("invalid_region"))?;
    let mut candidates = Vec::new();
    let mut source_lines = Vec::new();
    let mut skipped_lines = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let Some((account_id, name, password)) = parse_account_line(line) else {
            skipped_lines.push(index);
            continue;
        };
        source_lines.push(index);
        candidates.push(Account {
            account_id: account_id.to_owned(),
            name: name.to_owned(),
            region: region.to_owned(),
            region_display: region_label.clone(),
            password: password.to_owned(),
            description: String::new(),
            tier: "Unranked".to_owned(),
            reached_last_season: "N/A".to_owned(),
            finished_last_season: "N/A".to_owned(),
            ..Account::default()
        });
    }
    let result = manager(&state)?
        .add_accounts(candidates)
        .map_err(|error| fail_with("save_failed", error))?;
    skipped_lines.extend(result.skipped_indices.iter().map(|&index| source_lines[index]));
    skipped_lines.sort_unstable();
    let added = result.added.len();
    start_refresh(app, Arc::clone(&state), result.added, RefreshKind::Partial);
    Ok(BulkAddResult { added, skipped_lines })
}

#[tauri::command]
fn update_account(
    app: AppHandle,
    state: AppState<'_>,
    key: Key,
    name: String,
    description: String,
) -> CommandResult<AccountView> {
    let key = AccountKey::from(key);
    let name = name.trim().to_owned();
    let (updated, renamed) = {
        let mut manager = manager(&state)?;
        let account = manager.account_mut(&key).ok_or_else(|| fail("not_found"))?;
        let renamed = account.name != name;
        account.name = name;
        if renamed {
            account.riot_id_not_found = false;
            RankInfo::unranked().apply_to(account);
        }
        account.description = description.trim().to_owned();
        let updated = account.clone();
        manager
            .save_accounts()
            .map_err(|error| fail_with("save_failed", error))?;
        (updated, renamed)
    };
    let view = account_view(&updated);
    if renamed {
        start_refresh(app, Arc::clone(&state), vec![updated], RefreshKind::Partial);
    }
    Ok(view)
}

#[tauri::command]
fn delete_account(state: AppState<'_>, key: Key) -> CommandResult<()> {
    manager(&state)?
        .delete_account(&key.account_id, &key.region)
        .map_err(|error| fail_with("delete_failed", error))
}

#[tauri::command]
fn refresh_ranks(app: AppHandle, state: AppState<'_>) -> CommandResult<usize> {
    let accounts = manager(&state)?.accounts.clone();
    if accounts.is_empty() {
        return Err(fail("no_accounts"));
    }
    let count = accounts.len();
    if !start_refresh(app, Arc::clone(&state), accounts, RefreshKind::Manual) {
        return Err(fail("refresh_running"));
    }
    Ok(count)
}

#[tauri::command]
fn get_settings(app: AppHandle, state: AppState<'_>) -> SettingsView {
    settings_view(&app, &state)
}

#[tauri::command]
fn update_settings(app: AppHandle, state: AppState<'_>, input: SettingsInput) -> CommandResult<SettingsView> {
    {
        let mut settings = state.settings.lock().map_err(|_| fail("storage_locked"))?;
        settings.auto_refresh = input.auto_refresh;
        settings.interval_minutes = input
            .interval_minutes
            .clamp(MIN_INTERVAL_MINUTES, MAX_INTERVAL_MINUTES);
        settings.start_minimized = input.start_minimized;
        settings.login_method = input.login_method;
        settings.launch_game = input.launch_game;
        settings.check_updates = input.check_updates;
        settings.save(state.settings_path.as_ref());
    }
    let autolaunch = app.autolaunch();
    if autolaunch.is_enabled().unwrap_or(false) != input.launch_at_startup {
        let result = if input.launch_at_startup {
            autolaunch.enable()
        } else {
            autolaunch.disable()
        };
        result.map_err(|error| fail_with("autostart_failed", error))?;
    }
    let _ = app.emit("schedule", schedule(&state));
    Ok(settings_view(&app, &state))
}

fn set_clipboard(text: &str) -> CommandResult<()> {
    arboard::Clipboard::new()
        .and_then(|mut clipboard| leagueaccounts::clipboard::set_private_text(&mut clipboard, text))
        .map_err(|_| {
            logging::record(Event::ClipboardFailed, Reason::Other);
            fail("clipboard")
        })
}

#[tauri::command]
fn copy_account_id(state: AppState<'_>, key: Key) -> CommandResult<()> {
    let account = find_account(&state, key)?;
    set_clipboard(&account.account_id)?;
    state.clipboard_generation.fetch_add(1, Ordering::SeqCst);
    Ok(())
}

#[tauri::command]
fn copy_password(state: AppState<'_>, key: Key) -> CommandResult<u64> {
    let account = find_account(&state, key)?;
    let password = password_for(&account);
    if password.is_empty() {
        return Err(fail("no_password"));
    }
    set_clipboard(&password)?;
    // Wipe the password later, unless something else was copied meanwhile.
    let generation = state.clipboard_generation.fetch_add(1, Ordering::SeqCst) + 1;
    let shared = Arc::clone(&state);
    thread::spawn(move || {
        thread::sleep(PASSWORD_CLIPBOARD_TTL);
        if shared.clipboard_generation.load(Ordering::SeqCst) != generation {
            return;
        }
        if let Ok(mut clipboard) = arboard::Clipboard::new() {
            if clipboard.get_text().is_ok_and(|text| text == password) {
                let _ = clipboard.clear();
            }
        }
    });
    Ok(PASSWORD_CLIPBOARD_TTL.as_secs())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
enum LoginResult {
    /// Signed in and confirmed by the Riot Client (game launched if enabled).
    SignedIn,
    /// Credentials typed in explicit previous-window mode.
    Typed,
    /// This account was already signed in (game launched if enabled).
    AlreadySignedIn,
}

/// Sign in with the account. With the Riot method the client is opened (and
/// the game launched when enabled). A different signed-in account is signed out
/// automatically before connecting the selected account.
#[tauri::command]
async fn login(
    app: AppHandle,
    state: AppState<'_>,
    key: Key,
    tft: bool,
    close_running: bool,
) -> CommandResult<LoginResult> {
    let account = find_account(&state, key)?;
    let guard = state.login.try_start().ok_or_else(|| fail("login_busy"))?;
    let password = password_for(&account);
    let settings = state.settings.lock().map(|s| s.clone()).unwrap_or_default();
    let shared = Arc::clone(&state);
    let result = tauri::async_runtime::spawn_blocking(move || {
        // Acquired before queueing; released only after native renderer cleanup.
        let _guard = guard;
        if shared.login.cancel.load(Ordering::SeqCst) {
            return Err(LoginError::Cancelled);
        }
        logging::record(
            Event::LoginStarted,
            if settings.login_method == LoginMethod::Previous {
                Reason::PreviousWindow
            } else {
                Reason::RiotClient
            },
        );
        let mut progress = |step: LoginStep| {
            step.record();
            let _ = app.emit("login-step", step.code());
        };
        if settings.login_method == LoginMethod::Previous {
            progress(LoginStep::Focus);
            progress(LoginStep::Type);
            return if auto_type_credentials(&account.account_id, &password) {
                progress(LoginStep::Done);
                Ok(LoginOutcome::Typed)
            } else {
                Err(LoginError::TypingFailed)
            };
        }
        let game = settings.launch_game.then(|| {
            if tft && riot_client::game_installed(Game::Tft) {
                Game::Tft
            } else {
                Game::League
            }
        });
        riot_client::login(
            &account.account_id,
            &password,
            game,
            close_running,
            &mut progress,
            &shared.login.cancel,
        )
    })
    .await
    .unwrap_or(Err(LoginError::WorkerPanicked));
    riot_client::record_login_result(&result);
    result
        .map(|outcome| match outcome {
            LoginOutcome::SignedIn => LoginResult::SignedIn,
            LoginOutcome::Typed => LoginResult::Typed,
            LoginOutcome::AlreadySignedIn => LoginResult::AlreadySignedIn,
        })
        .map_err(|error| fail(error.code()))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GameStatusView {
    client_open: bool,
    in_game: bool,
    /// Whether Riot and any running game client both confirm this account.
    same_account: bool,
}

/// What is open right now, so the UI can warn before switching accounts.
#[tauri::command]
async fn game_status(state: AppState<'_>, key: Key) -> CommandResult<GameStatusView> {
    let account = find_account(&state, key)?;
    let status = tauri::async_runtime::spawn_blocking(riot_client::status)
        .await
        .unwrap_or_default();
    Ok(GameStatusView {
        client_open: status.client_open,
        in_game: status.in_game,
        same_account: status.signed_in_as(&account.account_id),
    })
}

#[tauri::command]
fn cancel_login(state: AppState<'_>) {
    state.login.request_cancel();
}

#[tauri::command]
async fn export_data(state: AppState<'_>, title: String) -> CommandResult<Option<String>> {
    let Some(path) = tauri::async_runtime::spawn_blocking(move || {
        rfd::FileDialog::new()
            .set_title(title)
            .set_file_name("credentials.json")
            .add_filter("JSON", &["json"])
            .save_file()
    })
    .await
    .map_err(|_| fail("dialog_failed"))?
    else {
        return Ok(None);
    };
    let json = manager(&state)?.export_accounts().map_err(|error| {
        logging::record(Event::ExportFailed, Reason::from_error(error.as_ref()));
        fail_with("export_failed", error)
    })?;
    std::fs::write(&path, json).map_err(|error| {
        logging::record(Event::ExportFailed, Reason::from_error(&error));
        fail_with("file_write", error)
    })?;
    Ok(Some(path.display().to_string()))
}

#[tauri::command]
async fn import_data(state: AppState<'_>, title: String) -> CommandResult<Option<BatchResult>> {
    let Some(path) = tauri::async_runtime::spawn_blocking(move || {
        rfd::FileDialog::new()
            .set_title(title)
            .add_filter("JSON", &["json"])
            .pick_file()
    })
    .await
    .map_err(|_| fail("dialog_failed"))?
    else {
        return Ok(None);
    };
    let json = std::fs::read_to_string(&path).map_err(|error| {
        logging::record(Event::ImportFailed, Reason::from_error(&error));
        fail_with("file_read", error)
    })?;
    let (added, skipped) = manager(&state)?.import_accounts(&json).map_err(|error| {
        logging::record(Event::ImportFailed, Reason::from_error(error.as_ref()));
        fail_with("import_failed", error)
    })?;
    Ok(Some(BatchResult { added, skipped }))
}

/// Open the account's OP.GG page (LoL or TFT) in the default browser.
#[tauri::command]
fn open_profile(state: AppState<'_>, key: Key, tft: bool) -> CommandResult<()> {
    let account = find_account(&state, key)?;
    if account.name.is_empty() {
        return Err(fail("riot_id_missing"));
    }
    let fetcher = RankFetcher::default();
    let url = if tft {
        fetcher.build_opgg_tft_url(&account.region, &account.name)
    } else {
        fetcher.build_opgg_url(&account.region, &account.name)
    };
    std::process::Command::new("explorer.exe")
        .arg(url)
        .spawn()
        .map(|_| ())
        .map_err(|_| fail("browser"))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct UpdateStatus {
    current: &'static str,
    latest: Option<Release>,
    newer: bool,
}

#[tauri::command]
async fn check_update() -> CommandResult<UpdateStatus> {
    let latest = tauri::async_runtime::spawn_blocking(updates::latest_release)
        .await
        .map_err(|_| fail("update_failed"))?
        .map_err(|error| fail_with("update_failed", error))?;
    let current = env!("CARGO_PKG_VERSION");
    let newer = latest
        .as_ref()
        .is_some_and(|release| updates::is_newer(&release.version, current));
    Ok(UpdateStatus { current, latest, newer })
}

/// Open a release page of this app's repository in the default browser.
#[tauri::command]
fn open_release(url: String) -> CommandResult<()> {
    if !updates::is_release_url(&url) {
        return Err(fail("browser"));
    }
    std::process::Command::new("explorer.exe")
        .arg(url)
        .spawn()
        .map(|_| ())
        .map_err(|_| fail("browser"))
}

#[tauri::command]
fn open_logs_folder() -> CommandResult<()> {
    // Fall back to the default location when file logging failed to start,
    // and recreate the folder if it was deleted while the app was running.
    let directory = logging::directory()
        .or_else(|| app_data_dir().ok().map(|directory| directory.join("logs")))
        .ok_or_else(|| fail("logs_unavailable"))?;
    std::fs::create_dir_all(&directory).map_err(|error| fail_with("logs_unavailable", error))?;
    std::process::Command::new("explorer.exe")
        .arg(directory)
        .spawn()
        .map(|_| ())
        .map_err(|error| {
            logging::record(Event::LogFolderOpenFailed, Reason::from_error(&error));
            fail("logs_folder")
        })
}

fn main() {
    logging::install_panic_hook();
    // A logging failure must not prevent users from opening their accounts.
    let _ = logging::init();

    let provider: Arc<dyn RankProvider> = Arc::new(RankFetcher::new());
    let mut manager = match AccountManager::new(provider) {
        Ok(manager) => manager,
        Err(_) => {
            logging::record(Event::AppRunFailed, Reason::Storage);
            std::process::exit(1);
        }
    };
    let load_error = manager
        .load_accounts()
        .err()
        .map(|error| fail_with("load_failed", error));
    let settings_path = settings_file().ok();
    let settings = Settings::load(settings_path.as_ref());
    let start_minimized = settings.start_minimized && std::env::args().any(|arg| arg == AUTOSTART_FLAG);

    let shared = Arc::new(Shared {
        manager: Mutex::new(manager),
        settings: Mutex::new(settings),
        settings_path,
        refresh_running: Mutex::new(false),
        clipboard_generation: AtomicU64::new(0),
        login: Arc::new(LoginControl::default()),
        load_error,
    });

    let result = tauri::Builder::default()
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            Some(vec![AUTOSTART_FLAG]),
        ))
        .plugin(login_control::plugin(Arc::clone(&shared.login)))
        .manage(Arc::clone(&shared))
        .invoke_handler(tauri::generate_handler![
            bootstrap,
            list_accounts,
            add_account,
            multi_add,
            update_account,
            delete_account,
            refresh_ranks,
            get_settings,
            update_settings,
            copy_account_id,
            copy_password,
            login,
            cancel_login,
            game_status,
            export_data,
            import_data,
            open_profile,
            check_update,
            open_release,
            open_logs_folder,
        ])
        .setup(move |app| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                if start_minimized {
                    let _ = window.minimize();
                } else {
                    let _ = window.set_focus();
                }
            }
            spawn_scheduler(app.handle().clone(), Arc::clone(&shared));
            spawn_update_checker(app.handle().clone(), Arc::clone(&shared));
            Ok(())
        })
        .run(tauri::generate_context!());
    if result.is_err() {
        logging::record(Event::AppRunFailed, Reason::Other);
        // Do not return the raw error to Rust's stderr termination handler.
        std::process::exit(1);
    }
    logging::record(Event::SessionEnded, Reason::None);
}
