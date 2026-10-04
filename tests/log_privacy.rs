//! This executable has one test, so the global logger and panic hook are
//! isolated from the other test binaries and the user's application data.

use leagueaccounts::logging::{self, Event, Reason};
use leagueaccounts::{Account, AccountManager, RankInfo, RankProvider};
use std::fs;
use std::sync::Arc;

const ACCOUNT_ID: &str = "PRIVATE_ACCOUNT_ID_a8c4";
const PASSWORD: &str = "PRIVATE_PASSWORD_f3b7";

struct PanickingProvider;

impl RankProvider for PanickingProvider {
    fn fetch_rank(&self, account: &Account) -> RankInfo {
        // Deliberately put credentials in the panic to exercise the real hook.
        panic!(
            "account={} password={}",
            account.account_id, account.password
        );
    }
}

#[test]
fn real_log_files_exclude_credentials_even_in_panics_and_raw_errors() {
    if std::env::var_os("LEAGUEACCOUNTS_LOG_TEST_CHILD").is_some() {
        exercise_logging(std::path::Path::new(&std::env::var_os("APPDATA").unwrap()));
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    // Configure app data before process startup. The child exits and closes
    // its global log handle before TempDir removes the files on Windows.
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "real_log_files_exclude_credentials_even_in_panics_and_raw_errors",
        ])
        .env("APPDATA", directory.path())
        .env("LEAGUEACCOUNTS_LOG_TEST_CHILD", "1")
        .output()
        .unwrap();
    assert!(output.status.success(), "logging privacy subprocess failed");
    for stream in [&output.stdout, &output.stderr] {
        let text = String::from_utf8_lossy(stream);
        assert!(!text.contains(ACCOUNT_ID));
        assert!(!text.contains(PASSWORD));
    }
}

fn exercise_logging(directory: &std::path::Path) {
    logging::install_panic_hook();
    logging::init().unwrap();
    assert!(logging::is_available());
    logging::record(Event::LoginStarted, Reason::RiotClient);
    leagueaccounts::riot_client::LoginStep::LaunchGame.record();
    leagueaccounts::riot_client::record_login_result(&Err(
        leagueaccounts::riot_client::LoginError::AccountMismatch,
    ));

    let path = directory.join(format!("{ACCOUNT_ID}-{PASSWORD}.json"));
    let mut manager = AccountManager::with_path(&path, Arc::new(PanickingProvider));
    manager.accounts.push(Account {
        account_id: ACCOUNT_ID.into(),
        password: PASSWORD.into(),
        name: ACCOUNT_ID.into(),
        description: PASSWORD.into(),
        ..Account::default()
    });
    manager.refresh_ranks().unwrap();
    assert_eq!(manager.accounts[0].tier, "Error");
    let malformed =
        format!(r#"[{{"account_id":"{ACCOUNT_ID}","password":"{PASSWORD}","region":false}}]"#);
    assert!(manager.import_accounts(&malformed).is_err());
    fs::write(&path, &malformed).unwrap();
    assert!(manager.load_accounts().is_err());
    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();
    assert!(manager.save_accounts().is_err());

    let raw_error = std::io::Error::new(
        std::io::ErrorKind::PermissionDenied,
        format!(
            "{ACCOUNT_ID}\n{PASSWORD}\npath={} body={malformed}",
            path.display()
        ),
    );
    logging::record(Event::ExportFailed, Reason::from_error(&raw_error));

    #[cfg(windows)]
    {
        // An oversized username is rejected by keyring's constructor before
        // any native credential is read, written or deleted.
        let username = ACCOUNT_ID.repeat(1000);
        let service = "LeagueAccounts-privacy-test";
        assert!(leagueaccounts::credentials::get_password(service, &username).is_err());
        assert!(leagueaccounts::credentials::set_password(service, &username, PASSWORD).is_err());
        assert!(leagueaccounts::credentials::delete_password(service, &username).is_err());
    }

    // A reqwest error includes a URL carrying credentials, query and path.
    // Building an invalid URL fails locally without network access.
    let error = reqwest::blocking::Client::new()
        .get(format!(
            "http://[{ACCOUNT_ID}:{PASSWORD}]/?password={PASSWORD}"
        ))
        .build()
        .unwrap_err();
    logging::record(Event::RankFetchFailed, Reason::from_request(&error));
    let worker = std::thread::Builder::new()
        .name(format!("{ACCOUNT_ID}-{PASSWORD}"))
        .spawn(|| panic!("{ACCOUNT_ID}\n{PASSWORD}"))
        .unwrap();
    assert!(worker.join().is_err());
    logging::record(Event::SessionEnded, Reason::None);

    let logs = fs::read_dir(logging::directory().unwrap())
        .unwrap()
        .map(|entry| fs::read_to_string(entry.unwrap().path()).unwrap())
        .collect::<Vec<_>>()
        .concat();
    for event in [
        "session_started",
        "login_started",
        "login_progress",
        "login_failed",
        "session_ended",
        "panic",
        "rank_worker_panicked",
        "import_failed",
        "accounts_load_failed",
        "accounts_save_failed",
        "export_failed",
        "rank_fetch_failed",
    ] {
        assert!(
            logs.contains(&format!("event={event} ")),
            "missing diagnostic event"
        );
    }
    #[cfg(windows)]
    for event in [
        "credential_read_failed",
        "credential_write_failed",
        "credential_delete_failed",
    ] {
        assert!(logs.contains(&format!("event={event} reason=credential_store")));
    }
    for secret in [ACCOUNT_ID, PASSWORD, path.to_str().unwrap(), &malformed] {
        assert!(
            !logs.contains(secret),
            "sensitive data reached the file sink"
        );
    }
    // Every line has only the seven allowlisted fields, including timestamps.
    assert!(logs
        .lines()
        .all(|line| line.split_whitespace().count() == 7));
    assert!(logs.contains("reason=permission_denied"));
    assert!(logs.contains(&format!("app_version={}", env!("CARGO_PKG_VERSION"))));
}
