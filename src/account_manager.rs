use crate::credentials;
use crate::logging::{self, Event, Reason};
use crate::models::{Account, AccountKey, RankInfo, TftRank};
use crate::rank_fetcher::RankProvider;
use crate::utils::{accounts_file, region_display, sort_accounts};
use serde::{Deserialize, Serialize};
use std::collections::{HashSet, VecDeque};
use std::error::Error;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;

pub const KEYRING_SERVICE: &str = credentials::SERVICE;

pub type ManagerResult<T> = Result<T, Box<dyn Error + Send + Sync>>;

pub struct AddAccountsResult {
    pub added: Vec<Account>,
    pub skipped_indices: Vec<usize>,
}

// Keep credential failures testable without touching the user's keyring.
trait PasswordStore {
    fn get(&self, key: &str) -> ManagerResult<Option<String>>;
    fn set(&self, key: &str, password: &str) -> ManagerResult<()>;
    fn delete(&self, key: &str) -> ManagerResult<()>;
}

struct NativePasswords;

impl PasswordStore for NativePasswords {
    fn get(&self, key: &str) -> ManagerResult<Option<String>> {
        credentials::get_password(KEYRING_SERVICE, key)
    }

    fn set(&self, key: &str, password: &str) -> ManagerResult<()> {
        credentials::set_password(KEYRING_SERVICE, key, password)
    }

    fn delete(&self, key: &str) -> ManagerResult<()> {
        credentials::delete_password(KEYRING_SERVICE, key)
    }
}

pub struct AccountManager {
    pub accounts: Vec<Account>,
    pub accounts_file: PathBuf,
    pub rank_fetcher: Arc<dyn RankProvider>,
}

impl AccountManager {
    pub fn new(rank_fetcher: Arc<dyn RankProvider>) -> ManagerResult<Self> {
        Ok(Self {
            accounts: Vec::new(),
            accounts_file: accounts_file().inspect_err(|error| {
                logging::record(Event::DataDirectoryFailed, Reason::from_error(error));
            })?,
            rank_fetcher,
        })
    }

    pub fn with_path(path: impl Into<PathBuf>, rank_fetcher: Arc<dyn RankProvider>) -> Self {
        Self {
            accounts: Vec::new(),
            accounts_file: path.into(),
            rank_fetcher,
        }
    }

    pub fn load_accounts(&mut self) -> ManagerResult<()> {
        self.accounts.clear();
        if !self.accounts_file.exists() {
            return Ok(());
        }
        let contents = std::fs::read_to_string(&self.accounts_file).inspect_err(|error| {
            logging::record(Event::AccountsLoadFailed, Reason::from_error(error));
        })?;
        let mut loaded: Vec<Account> = serde_json::from_str(&contents).inspect_err(|error| {
            logging::record(Event::AccountsLoadFailed, Reason::from_error(error));
        })?;
        for account in &mut loaded {
            if account.region_display.is_empty() {
                account.region_display = region_display(&account.region);
            }
            account.password = credentials::get_password(
                KEYRING_SERVICE,
                &format!("{}:{}", account.region, account.account_id),
            )
            .ok()
            .flatten()
            .unwrap_or_default();
        }
        self.accounts = loaded;
        sort_accounts(&mut self.accounts);
        Ok(())
    }

    pub fn save_accounts(&self) -> ManagerResult<()> {
        self.persist_accounts(self.prepare_accounts(&self.accounts)?)
    }

    fn prepare_accounts(&self, accounts: &[Account]) -> ManagerResult<tempfile::NamedTempFile> {
        let result = (|| -> ManagerResult<_> {
            let parent = self
                .accounts_file
                .parent()
                .filter(|path| !path.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."));
            std::fs::create_dir_all(parent)?;
            let json = serde_json::to_vec_pretty(accounts)?;
            // Unique sibling files avoid colliding with another app instance.
            // Dropping a failed write removes its temporary file automatically.
            let mut file = tempfile::NamedTempFile::new_in(parent)?;
            file.write_all(&json)?;
            file.as_file().sync_all()?;
            Ok(file)
        })();
        result.inspect_err(|error| {
            logging::record(
                Event::AccountsSaveFailed,
                Reason::from_error(error.as_ref()),
            );
        })
    }

    fn persist_accounts(&self, file: tempfile::NamedTempFile) -> ManagerResult<()> {
        file.persist(&self.accounts_file).map_err(|error| {
            logging::record(Event::AccountsSaveFailed, Reason::from_error(&error.error));
            Box::new(error.error) as Box<dyn Error + Send + Sync>
        })?;
        Ok(())
    }

    /// Store accepted accounts in one write; publish them in memory only after
    /// the file has been replaced. Skipped indices refer to the input vector.
    pub fn add_accounts(&mut self, accounts: Vec<Account>) -> ManagerResult<AddAccountsResult> {
        self.add_accounts_with(accounts, &NativePasswords)
    }

    fn add_accounts_with(
        &mut self,
        accounts: Vec<Account>,
        passwords: &impl PasswordStore,
    ) -> ManagerResult<AddAccountsResult> {
        let mut keys: HashSet<_> = self
            .accounts
            .iter()
            .map(|account| {
                (
                    account.region.clone(),
                    account.account_id.to_ascii_lowercase(),
                )
            })
            .collect();
        let mut added = Vec::new();
        let mut skipped_indices = Vec::new();
        let mut previous_passwords = Vec::new();
        for (index, account) in accounts.into_iter().enumerate() {
            let key = (
                account.region.clone(),
                account.account_id.to_ascii_lowercase(),
            );
            if keys.contains(&key) {
                skipped_indices.push(index);
                continue;
            }
            if !account.password.is_empty() {
                let credential_key = format!("{}:{}", account.region, account.account_id);
                let stored = passwords.get(&credential_key).and_then(|previous| {
                    passwords.set(&credential_key, &account.password)?;
                    Ok(previous)
                });
                match stored {
                    Ok(previous) => previous_passwords.push((credential_key, previous)),
                    Err(_) => {
                        skipped_indices.push(index);
                        continue;
                    }
                }
            }
            keys.insert(key);
            added.push(account);
        }
        if !added.is_empty() {
            let mut next = self.accounts.clone();
            next.extend(added.iter().cloned());
            sort_accounts(&mut next);
            let saved = self
                .prepare_accounts(&next)
                .and_then(|file| self.persist_accounts(file));
            if let Err(error) = saved {
                // Restore overwritten credentials as well as removing new ones.
                // Attempt every rollback even if an individual restore fails.
                let mut rollback_error = None;
                for (key, previous) in previous_passwords.into_iter().rev() {
                    let restored = match previous {
                        Some(password) => passwords.set(&key, &password),
                        None => passwords.delete(&key),
                    };
                    if let Err(error) = restored {
                        rollback_error.get_or_insert(error);
                    }
                }
                return Err(rollback_error.unwrap_or(error));
            }
            self.accounts = next;
        }
        Ok(AddAccountsResult {
            added,
            skipped_indices,
        })
    }

    pub fn delete_account(&mut self, account_id: &str, region: &str) -> ManagerResult<()> {
        self.delete_account_with(account_id, region, &NativePasswords)
    }

    fn delete_account_with(
        &mut self,
        account_id: &str,
        region: &str,
        passwords: &impl PasswordStore,
    ) -> ManagerResult<()> {
        let mut next = self.accounts.clone();
        next.retain(|account| !(account.account_id == account_id && account.region == region));
        if next.len() == self.accounts.len() {
            return Ok(());
        }
        // Prepare the replacement before deleting anything from the keyring.
        let file = self.prepare_accounts(&next)?;
        let key = format!("{region}:{account_id}");
        let previous = passwords.get(&key)?;
        passwords.delete(&key)?;
        if let Err(error) = self.persist_accounts(file) {
            if let Some(password) = previous {
                passwords.set(&key, &password)?;
            }
            return Err(error);
        }
        self.accounts = next;
        Ok(())
    }

    pub fn account(&self, key: &AccountKey) -> Option<&Account> {
        self.accounts.iter().find(|account| account.key() == *key)
    }

    pub fn account_mut(&mut self, key: &AccountKey) -> Option<&mut Account> {
        self.accounts
            .iter_mut()
            .find(|account| account.key() == *key)
    }

    pub fn apply_rank_info(&mut self, fetched: &Account, info: &RankInfo) -> bool {
        if let Some(account) = self
            .account_mut(&fetched.key())
            .filter(|account| account.name == fetched.name)
        {
            info.apply_to(account);
            return true;
        }
        false
    }

    /// Fetch all ranks with at most four concurrent provider calls.
    pub fn refresh_ranks(&mut self) -> ManagerResult<()> {
        logging::record(Event::RankRefreshStarted, Reason::None);
        let jobs: Vec<(usize, Account)> = self.accounts.iter().cloned().enumerate().collect();
        let results = run_rank_jobs(self.rank_fetcher.clone(), jobs);
        for (index, info) in results {
            if let Some(account) = self.accounts.get_mut(index) {
                info.apply_to(account);
            }
        }
        sort_accounts(&mut self.accounts);
        self.save_accounts()?;
        logging::record(Event::RankRefreshCompleted, Reason::None);
        Ok(())
    }

    /// Serialize every field, including passwords, for an explicit export.
    pub fn export_accounts(&mut self) -> ManagerResult<String> {
        let mut export = Vec::with_capacity(self.accounts.len());
        for account in &mut self.accounts {
            if account.password.is_empty() {
                account.password = credentials::get_password(
                    KEYRING_SERVICE,
                    &format!("{}:{}", account.region, account.account_id),
                )
                .ok()
                .flatten()
                .unwrap_or_default();
            }
            export.push(ExportAccount::from(&*account));
        }
        Ok(serde_json::to_string_pretty(&export).inspect_err(|error| {
            logging::record(Event::ExportFailed, Reason::from_error(error));
        })?)
    }

    /// Import a JSON array. Returns `(added, skipped)` like the original app.
    pub fn import_accounts(&mut self, json: &str) -> ManagerResult<(usize, usize)> {
        self.import_accounts_with(json, &NativePasswords)
    }

    fn import_accounts_with(
        &mut self,
        json: &str,
        passwords: &impl PasswordStore,
    ) -> ManagerResult<(usize, usize)> {
        let data: Vec<ImportAccount> = serde_json::from_str(json).inspect_err(|error| {
            logging::record(Event::ImportFailed, Reason::from_error(error));
        })?;
        let mut accounts = Vec::with_capacity(data.len());
        let mut skipped = 0;
        for item in data {
            let account_id = item.account_id.trim().to_owned();
            let region = item.region.trim().to_owned();
            if account_id.is_empty() || region.is_empty() {
                skipped += 1;
                continue;
            }
            let account = Account {
                account_id: account_id.clone(),
                name: item.name,
                riot_id_not_found: item.riot_id_not_found,
                region: region.clone(),
                region_display: if item.region_display.is_empty() {
                    region_display(&region)
                } else {
                    item.region_display
                },
                password: item.password,
                description: item.description,
                tier: if item.tier.is_empty() {
                    "Unranked".to_owned()
                } else {
                    item.tier
                },
                division: item.division,
                lp: item.lp,
                level: item.level,
                reached_last_season: if item.reached_last_season.is_empty() {
                    "N/A".to_owned()
                } else {
                    item.reached_last_season
                },
                finished_last_season: if item.finished_last_season.is_empty() {
                    "N/A".to_owned()
                } else {
                    item.finished_last_season
                },
                tft: item.tft,
            };
            accounts.push(account);
        }
        let result = self.add_accounts_with(accounts, passwords)?;
        Ok((result.added.len(), skipped + result.skipped_indices.len()))
    }
}

fn run_rank_jobs(
    provider: Arc<dyn RankProvider>,
    jobs: Vec<(usize, Account)>,
) -> Vec<(usize, RankInfo)> {
    if jobs.is_empty() {
        return Vec::new();
    }
    let queue = Arc::new(Mutex::new(VecDeque::from(jobs)));
    let (sender, receiver) = mpsc::channel();
    let worker_count = queue.lock().map(|queue| queue.len().min(4)).unwrap_or(1);
    let mut workers = Vec::with_capacity(worker_count);
    for _ in 0..worker_count {
        let queue = Arc::clone(&queue);
        let sender = sender.clone();
        let provider = Arc::clone(&provider);
        workers.push(thread::spawn(move || loop {
            let job = queue.lock().ok().and_then(|mut queue| queue.pop_front());
            let Some((index, account)) = job else { break };
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                provider.fetch_rank(&account)
            }))
            .unwrap_or_else(|_| {
                logging::record(Event::RankWorkerPanicked, Reason::None);
                RankInfo::error()
            });
            if sender.send((index, result)).is_err() {
                break;
            }
        }));
    }
    drop(sender);
    let mut results = Vec::new();
    for result in receiver {
        results.push(result);
    }
    for worker in workers {
        let _ = worker.join();
    }
    results.sort_by_key(|(index, _)| *index);
    results
}

#[derive(Clone, Debug, Serialize)]
struct ExportAccount {
    account_id: String,
    name: String,
    riot_id_not_found: bool,
    region: String,
    region_display: String,
    password: String,
    description: String,
    tier: String,
    division: String,
    lp: String,
    level: String,
    reached_last_season: String,
    finished_last_season: String,
    tft: TftRank,
}

impl From<&Account> for ExportAccount {
    fn from(account: &Account) -> Self {
        Self {
            account_id: account.account_id.clone(),
            name: account.name.clone(),
            riot_id_not_found: account.riot_id_not_found,
            region: account.region.clone(),
            region_display: account.region_display.clone(),
            password: account.password.clone(),
            description: account.description.clone(),
            tier: account.tier.clone(),
            division: account.division.clone(),
            lp: account.lp.clone(),
            level: account.level.clone(),
            reached_last_season: account.reached_last_season.clone(),
            finished_last_season: account.finished_last_season.clone(),
            tft: account.tft.clone(),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
struct ImportAccount {
    #[serde(default)]
    account_id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    riot_id_not_found: bool,
    #[serde(default)]
    region: String,
    #[serde(default)]
    region_display: String,
    #[serde(default)]
    password: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    tier: String,
    #[serde(default)]
    division: String,
    #[serde(default)]
    lp: String,
    #[serde(default)]
    level: String,
    #[serde(default)]
    reached_last_season: String,
    #[serde(default)]
    finished_last_season: String,
    #[serde(default)]
    tft: TftRank,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    #[derive(Default)]
    struct TestPasswords {
        values: RefCell<HashMap<String, String>>,
        fail_set: Option<String>,
        fail_delete: Option<String>,
    }

    impl PasswordStore for TestPasswords {
        fn get(&self, key: &str) -> ManagerResult<Option<String>> {
            Ok(self.values.borrow().get(key).cloned())
        }

        fn set(&self, key: &str, password: &str) -> ManagerResult<()> {
            if self.fail_set.as_deref() == Some(key) {
                return Err(std::io::Error::other("credential write refused").into());
            }
            self.values.borrow_mut().insert(key.into(), password.into());
            Ok(())
        }

        fn delete(&self, key: &str) -> ManagerResult<()> {
            if self.fail_delete.as_deref() == Some(key) {
                return Err(std::io::Error::other("credential deletion refused").into());
            }
            self.values.borrow_mut().remove(key);
            Ok(())
        }
    }

    fn test_account(id: &str) -> Account {
        Account {
            account_id: id.into(),
            region: "euw".into(),
            password: "dummy-password".into(),
            ..Account::default()
        }
    }

    fn test_manager(path: impl Into<PathBuf>) -> AccountManager {
        AccountManager::with_path(path, Arc::new(crate::rank_fetcher::RankFetcher::default()))
    }

    struct SlowProvider {
        active: AtomicUsize,
        max_active: AtomicUsize,
    }

    impl RankProvider for SlowProvider {
        fn fetch_rank(&self, _account: &Account) -> RankInfo {
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_active.fetch_max(active, Ordering::SeqCst);
            thread::sleep(Duration::from_millis(20));
            self.active.fetch_sub(1, Ordering::SeqCst);
            RankInfo {
                riot_id_not_found: Some(false),
                tier: "Gold".into(),
                division: "II".into(),
                lp: "50".into(),
                level: "100".into(),
                reached_last_season: "Platinum IV".into(),
                finished_last_season: "Gold I".into(),
                tft: None,
            }
        }
    }

    #[test]
    fn a_lookup_for_an_old_riot_id_cannot_overwrite_the_edited_account() {
        let directory = tempfile::tempdir().unwrap();
        let mut manager = AccountManager::with_path(
            directory.path().join("accounts.json"),
            Arc::new(crate::rank_fetcher::RankFetcher::default()),
        );
        let fetched = Account {
            account_id: "user".into(),
            name: "Old Name#TAG".into(),
            region: "euw".into(),
            ..Account::default()
        };
        manager.accounts.push(fetched.clone());
        manager.accounts[0].name.clear();
        let info = RankInfo {
            tier: "Diamond".into(),
            ..RankInfo::default()
        };
        assert!(!manager.apply_rank_info(&fetched, &info));
        assert_eq!(manager.accounts[0].tier, fetched.tier);
        manager.accounts[0].name = "New Name#TAG".into();
        assert!(!manager.apply_rank_info(&fetched, &info));
        let current = manager.accounts[0].clone();
        assert!(manager.apply_rank_info(&current, &info));
        assert_eq!(manager.accounts[0].tier, "Diamond");
    }

    #[test]
    fn refreshes_in_parallel_with_four_workers() {
        let provider = Arc::new(SlowProvider {
            active: AtomicUsize::new(0),
            max_active: AtomicUsize::new(0),
        });
        let path = tempfile::tempdir().unwrap().path().join("accounts.json");
        let mut manager = AccountManager::with_path(path, provider.clone());
        manager.accounts = (0..4)
            .map(|index| Account {
                account_id: index.to_string(),
                name: format!("Player{index}#EUW"),
                region: "euw".into(),
                region_display: "EUW".into(),
                tier: "Unranked".into(),
                reached_last_season: "N/A".into(),
                finished_last_season: "N/A".into(),
                ..Account::default()
            })
            .collect();
        manager.refresh_ranks().unwrap();
        assert_eq!(provider.max_active.load(Ordering::SeqCst), 4);
        assert!(manager
            .accounts
            .iter()
            .all(|account| account.tier == "Gold"));
    }

    #[test]
    fn saved_accounts_never_include_passwords() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("accounts.json");
        let provider = Arc::new(SlowProvider {
            active: AtomicUsize::new(0),
            max_active: AtomicUsize::new(0),
        });
        let mut manager = AccountManager::with_path(path.clone(), provider);
        manager.accounts.push(Account {
            account_id: "id".into(),
            password: "secret".into(),
            ..Account::default()
        });
        manager.save_accounts().unwrap();
        assert!(!std::fs::read_to_string(path).unwrap().contains("secret"));
    }

    #[test]
    fn saves_replace_existing_data_and_clean_up_failed_temporary_files() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("accounts.json");
        std::fs::write(&path, "old data").unwrap();
        let mut manager = test_manager(&path);
        manager.accounts.push(test_account("saved"));
        manager.save_accounts().unwrap();
        let saved: Vec<Account> = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved[0].account_id, "saved");
        assert!(saved[0].password.is_empty());

        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            // A reader denying file replacement must leave the old JSON intact.
            let original = std::fs::read(&path).unwrap();
            let locked = std::fs::OpenOptions::new()
                .read(true)
                .share_mode(1) // FILE_SHARE_READ, without FILE_SHARE_DELETE
                .open(&path)
                .unwrap();
            manager.accounts.push(test_account("unsaved"));
            assert!(manager.save_accounts().is_err());
            assert_eq!(std::fs::read(&path).unwrap(), original);
            drop(locked);
        }

        let blocked = directory.path().join("blocked.json");
        std::fs::create_dir(&blocked).unwrap();
        std::fs::write(blocked.join("sentinel"), "untouched").unwrap();
        manager.accounts_file = blocked.clone();
        assert!(manager.save_accounts().is_err());
        assert_eq!(
            std::fs::read_to_string(blocked.join("sentinel")).unwrap(),
            "untouched"
        );
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 2);
    }

    #[test]
    fn batch_skips_duplicates_and_failed_credentials_without_losing_input_indices() {
        let directory = tempfile::tempdir().unwrap();
        let mut manager = test_manager(directory.path().join("accounts.json"));
        let existing = test_account("existing");
        manager.accounts.push(existing.clone());
        let passwords = TestPasswords {
            fail_set: Some("euw:broken".into()),
            ..TestPasswords::default()
        };
        passwords.set("euw:existing", "original").unwrap();
        let mut other_region = test_account("new");
        other_region.region = "kr".into();
        let result = manager
            .add_accounts_with(
                vec![
                    test_account("EXISTING"),
                    test_account("new"),
                    test_account("NEW"),
                    test_account("broken"),
                    other_region,
                ],
                &passwords,
            )
            .unwrap();
        assert_eq!(result.skipped_indices, [0, 2, 3]);
        assert_eq!(result.added.len(), 2);
        assert_eq!(manager.accounts.len(), 3);
        assert!(manager.accounts.contains(&existing));
        assert_eq!(
            passwords.get("euw:existing").unwrap().as_deref(),
            Some("original")
        );
        assert_eq!(
            passwords.get("euw:new").unwrap().as_deref(),
            Some("dummy-password")
        );
        assert!(passwords.get("euw:NEW").unwrap().is_none());
        assert!(passwords.get("euw:broken").unwrap().is_none());
        assert!(passwords.get("kr:new").unwrap().is_some());
        let saved: Vec<Account> =
            serde_json::from_slice(&std::fs::read(&manager.accounts_file).unwrap()).unwrap();
        assert_eq!(saved.len(), 3);
    }

    #[test]
    fn failed_batch_save_restores_previous_credentials_and_memory() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("blocked.json");
        std::fs::create_dir(&path).unwrap();
        let mut manager = test_manager(&path);
        manager.accounts.push(test_account("existing"));
        let original = manager.accounts.clone();
        let passwords = TestPasswords::default();
        passwords
            .set("euw:overwritten", "original-password")
            .unwrap();
        let original_passwords = passwords.values.borrow().clone();
        assert!(manager
            .add_accounts_with(
                vec![test_account("overwritten"), test_account("new")],
                &passwords
            )
            .is_err());
        assert_eq!(manager.accounts, original);
        assert_eq!(*passwords.values.borrow(), original_passwords);
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);

        // One failed cleanup must not stop restoration of the other entry.
        let passwords = TestPasswords {
            fail_delete: Some("euw:new".into()),
            ..TestPasswords::default()
        };
        passwords
            .set("euw:overwritten", "original-password")
            .unwrap();
        assert!(manager
            .add_accounts_with(
                vec![test_account("overwritten"), test_account("new")],
                &passwords
            )
            .is_err());
        assert_eq!(manager.accounts, original);
        assert_eq!(
            passwords.get("euw:overwritten").unwrap().as_deref(),
            Some("original-password")
        );
    }

    #[test]
    fn import_skips_unstored_passwords_and_preserves_passwordless_accounts() {
        let directory = tempfile::tempdir().unwrap();
        let mut manager = test_manager(directory.path().join("accounts.json"));
        let passwords = TestPasswords {
            fail_set: Some("euw:broken".into()),
            ..TestPasswords::default()
        };
        let json = r#"[
            {"account_id":"valid","region":"euw","password":"dummy"},
            {"account_id":"broken","region":"euw","password":"dummy"},
            {"account_id":"VALID","region":"euw","password":"different"},
            {"account_id":"","region":"euw"},
            {"account_id":"metadata","region":"euw"}
        ]"#;
        assert_eq!(
            manager.import_accounts_with(json, &passwords).unwrap(),
            (2, 3)
        );
        assert_eq!(manager.accounts.len(), 2);
        assert_eq!(
            passwords.get("euw:valid").unwrap().as_deref(),
            Some("dummy")
        );
        assert!(passwords.get("euw:broken").unwrap().is_none());
        assert!(manager
            .accounts
            .iter()
            .any(|account| account.account_id == "metadata" && account.password.is_empty()));
    }

    #[test]
    fn failed_deletion_keeps_the_account_file_and_password() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("accounts.json");
        let mut manager = test_manager(&path);
        manager.accounts.push(test_account("kept"));
        manager.save_accounts().unwrap();
        let original = manager.accounts.clone();
        let original_file = std::fs::read(&path).unwrap();
        let mut passwords = TestPasswords {
            fail_delete: Some("euw:kept".into()),
            ..TestPasswords::default()
        };
        passwords.set("euw:kept", "stored-password").unwrap();
        assert!(manager
            .delete_account_with("kept", "euw", &passwords)
            .is_err());
        assert_eq!(manager.accounts, original);
        assert_eq!(std::fs::read(&path).unwrap(), original_file);
        assert_eq!(
            passwords.get("euw:kept").unwrap().as_deref(),
            Some("stored-password")
        );
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);

        passwords.fail_delete = None;
        let blocked = directory.path().join("blocked.json");
        std::fs::create_dir(&blocked).unwrap();
        manager.accounts_file = blocked;
        assert!(manager
            .delete_account_with("kept", "euw", &passwords)
            .is_err());
        assert_eq!(manager.accounts, original);
        assert_eq!(
            passwords.get("euw:kept").unwrap().as_deref(),
            Some("stored-password")
        );

        manager.accounts_file = path.clone();
        manager
            .delete_account_with("kept", "euw", &passwords)
            .unwrap();
        assert!(manager.accounts.is_empty());
        assert!(passwords.get("euw:kept").unwrap().is_none());
        assert_eq!(std::fs::read_to_string(path).unwrap(), "[]");
    }

    #[test]
    fn an_entirely_skipped_batch_does_not_create_a_file() {
        let directory = tempfile::tempdir().unwrap();
        let mut manager = test_manager(directory.path().join("missing").join("accounts.json"));
        let passwords = TestPasswords {
            fail_set: Some("euw:broken".into()),
            ..TestPasswords::default()
        };
        let result = manager
            .add_accounts_with(vec![test_account("broken")], &passwords)
            .unwrap();
        assert!(result.added.is_empty());
        assert_eq!(result.skipped_indices, [0]);
        assert!(!manager.accounts_file.exists());
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
    }

    #[test]
    fn storage_and_import_errors_never_log_secret_input_or_paths() {
        let directory = tempfile::tempdir().unwrap();
        let account_id = "PRIVATE_ACCOUNT_ID_7e5f";
        let password = "PRIVATE_PASSWORD_23b1";
        let path = directory
            .path()
            .join(format!("{account_id}-{password}.json"));
        let provider = Arc::new(SlowProvider {
            active: AtomicUsize::new(0),
            max_active: AtomicUsize::new(0),
        });
        let mut manager = AccountManager::with_path(&path, provider);
        let json = format!(
            r#"[{{"account_id":"{account_id}","password":"{password}","name": "{account_id}","region":"euw","tier":false}}]"#
        );
        // Deserialization's detailed error can echo attacker-controlled data.
        std::fs::write(&path, &json).unwrap();
        let logs = logging::capture(|| {
            assert!(manager.load_accounts().is_err());
            assert!(manager.import_accounts(&json).is_err());
            manager.accounts.push(Account {
                account_id: account_id.into(),
                password: password.into(),
                description: password.into(),
                ..Account::default()
            });
            // An existing directory at the save destination makes writing fail.
            std::fs::remove_file(&path).unwrap();
            std::fs::create_dir(&path).unwrap();
            assert!(manager.save_accounts().is_err());
        });
        for event in [
            "accounts_load_failed",
            "import_failed",
            "accounts_save_failed",
        ] {
            assert!(logs.contains(event));
        }
        for secret in [account_id, password, path.to_str().unwrap(), &json] {
            assert!(!logs.contains(secret));
        }
    }
}
