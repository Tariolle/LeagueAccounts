use crate::credentials;
use crate::locales;
use crate::logging::{self, Event, Reason};
use crate::models::{Account, AccountKey, RankInfo, RankProvenance, TftRank};
use crate::rank_fetcher::RankProvider;
use crate::utils::{accounts_file, region_display, sort_accounts, REGION_MAP};
use serde::{Deserialize, Serialize};
use serde::ser::{SerializeSeq, Serializer as _};
use std::collections::{HashMap, HashSet, VecDeque};
use std::error::Error;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;

pub const KEYRING_SERVICE: &str = credentials::SERVICE;

pub type ManagerResult<T> = Result<T, Box<dyn Error + Send + Sync>>;

/// Everything an explicit account edit may change. A `None` or empty password
/// keeps the stored credential, so an untouched field cannot wipe it.
#[derive(Clone, Debug)]
pub struct AccountChanges {
    pub name: String,
    pub description: String,
    pub locale: Option<String>,
    pub skip_profile_update: bool,
    pub password: Option<String>,
}

/// One account's admission to a rank refresh. The private generation makes a
/// late result invalid once the account is deleted, renamed, or refreshed
/// again.
pub struct RankJob {
    pub account: Account,
    generation: u64,
}

pub struct AddAccountsResult {
    pub added: Vec<Account>,
    pub skipped_indices: Vec<usize>,
}

// Keep credential failures testable without touching the user's keyring.
trait PasswordStore {
    fn get(&self, key: &str) -> ManagerResult<Option<String>>;
    fn set(&self, key: &str, password: &str) -> ManagerResult<()>;
    fn delete(&self, key: &str) -> ManagerResult<()>;

    fn snapshot(&self, key: &str) -> ManagerResult<credentials::PasswordSnapshot> {
        Ok(credentials::PasswordSnapshot {
            primary: self.get(key)?,
            legacy: None,
        })
    }

    fn restore(&self, key: &str, snapshot: credentials::PasswordSnapshot) -> ManagerResult<()> {
        match snapshot.primary {
            Some(password) => self.set(key, &password),
            None => self.delete(key),
        }
    }
}

struct NativePasswords<'a>(&'a str);

impl PasswordStore for NativePasswords<'_> {
    fn get(&self, key: &str) -> ManagerResult<Option<String>> {
        credentials::get_password(self.0, key)
    }

    fn set(&self, key: &str, password: &str) -> ManagerResult<()> {
        credentials::set_password(self.0, key, password)
    }

    fn delete(&self, key: &str) -> ManagerResult<()> {
        credentials::delete_password(self.0, key)
    }

    fn snapshot(&self, key: &str) -> ManagerResult<credentials::PasswordSnapshot> {
        credentials::snapshot_password(self.0, key)
    }

    fn restore(&self, key: &str, snapshot: credentials::PasswordSnapshot) -> ManagerResult<()> {
        credentials::restore_password(self.0, key, snapshot)
    }
}

pub struct AccountManager {
    pub accounts: Vec<Account>,
    pub accounts_file: PathBuf,
    pub rank_fetcher: Arc<dyn RankProvider>,
    /// Set when the stored file could not be read or parsed. Persistent
    /// mutations are refused until an explicit reload succeeds, so a partial
    /// in-memory list can never replace a file we failed to understand.
    load_failed: bool,
    rank_generation: u64,
    rank_jobs: HashMap<AccountKey, u64>,
}

impl AccountManager {
    pub fn new(rank_fetcher: Arc<dyn RankProvider>) -> ManagerResult<Self> {
        Ok(Self {
            accounts: Vec::new(),
            accounts_file: accounts_file().inspect_err(|error| {
                logging::record(Event::DataDirectoryFailed, Reason::from_error(error));
            })?,
            rank_fetcher,
            load_failed: false,
            rank_generation: 0,
            rank_jobs: HashMap::new(),
        })
    }

    pub fn with_path(path: impl Into<PathBuf>, rank_fetcher: Arc<dyn RankProvider>) -> Self {
        Self {
            accounts: Vec::new(),
            accounts_file: path.into(),
            rank_fetcher,
            load_failed: false,
            rank_generation: 0,
            rank_jobs: HashMap::new(),
        }
    }

    pub fn load_accounts(&mut self) -> ManagerResult<()> {
        self.load_accounts_with_service(KEYRING_SERVICE)
    }

    fn load_accounts_with_service(&mut self, service: &str) -> ManagerResult<()> {
        let contents = match std::fs::read(&self.accounts_file) {
            Ok(contents) => contents,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                // A file that has never existed is a new, empty, writable store.
                self.accounts.clear();
                self.rank_jobs.clear();
                self.load_failed = false;
                return Ok(());
            }
            Err(error) => {
                logging::record(Event::AccountsLoadFailed, Reason::from_error(&error));
                // Reading failed, so nothing here can honestly be called a
                // backup. Keep the old memory and refuse later writes.
                self.load_failed = true;
                return Err(Box::new(error));
            }
        };
        let mut loaded: Vec<Account> = match serde_json::from_slice(&contents) {
            Ok(loaded) => loaded,
            Err(error) => {
                logging::record(Event::AccountsLoadFailed, Reason::from_error(&error));
                // Damage is visible only after a successful read: preserve the
                // readable file under a name that cannot replace another backup.
                let _ = self.back_up_corrupt_accounts(&contents);
                self.load_failed = true;
                return Err(Box::new(error));
            }
        };
        for account in &mut loaded {
            if account.region_display.is_empty() {
                account.region_display = region_display(&account.region);
            }
            account.password = credentials::load_password_with_migration(
                service,
                &format!("{}:{}", account.region, account.account_id),
            )
            .ok()
            .flatten()
            .unwrap_or_default();
        }
        self.accounts = loaded;
        self.load_failed = false;
        self.rank_jobs.clear();
        sort_accounts(&mut self.accounts);
        Ok(())
    }

    /// Copy a readable but unparseable file beside itself under the first free
    /// `.corrupt[.N]` name. An existing file is never replaced.
    fn back_up_corrupt_accounts(&self, contents: &[u8]) -> ManagerResult<PathBuf> {
        let parent = self
            .accounts_file
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let Some(name) = self.accounts_file.file_name() else {
            return Err(std::io::Error::other("accounts file has no name").into());
        };
        for suffix in 0..=u32::MAX {
            let candidate = if suffix == 0 {
                format!("{}.corrupt", name.to_string_lossy())
            } else {
                format!("{}.corrupt.{suffix}", name.to_string_lossy())
            };
            let candidate = parent.join(candidate);
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&candidate)
            {
                Ok(mut file) => {
                    if let Err(error) = file.write_all(contents).and_then(|()| file.sync_all()) {
                        drop(file);
                        let _ = std::fs::remove_file(&candidate);
                        return Err(Box::new(error));
                    }
                    return Ok(candidate);
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(Box::new(error)),
            }
        }
        Err(std::io::Error::other("no free backup name").into())
    }

    pub fn save_accounts(&self) -> ManagerResult<()> {
        self.persist_accounts(self.prepare_accounts(&self.accounts)?)
    }

    fn ensure_loaded(&self) -> ManagerResult<()> {
        if self.load_failed {
            return Err(std::io::Error::other(
                "stored accounts have not loaded successfully",
            )
            .into());
        }
        Ok(())
    }

    fn prepare_accounts<'a>(
        &self,
        accounts: impl IntoIterator<Item = &'a Account>,
    ) -> ManagerResult<tempfile::NamedTempFile> {
        let result = (|| -> ManagerResult<_> {
            self.ensure_loaded()?;
            let parent = self.accounts_file
                .parent()
                .filter(|path| !path.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."));
            std::fs::create_dir_all(parent)?;
            // Stream references into a unique sibling file: no complete JSON
            // buffer or duplicate account/password list is needed.
            let mut file = tempfile::NamedTempFile::new_in(parent)?;
            {
                let mut serializer = serde_json::Serializer::pretty(file.as_file_mut());
                let mut sequence = serializer.serialize_seq(None)?;
                for account in accounts {
                    sequence.serialize_element(account)?;
                }
                sequence.end()?;
            }
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
        self.add_accounts_with(accounts, &NativePasswords(KEYRING_SERVICE))
    }

    fn add_accounts_with(
        &mut self,
        accounts: Vec<Account>,
        passwords: &impl PasswordStore,
    ) -> ManagerResult<AddAccountsResult> {
        self.ensure_loaded()?;
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
                let stored = passwords.snapshot(&credential_key).and_then(|previous| {
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
                    let restored = passwords.restore(&key, previous);
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

    /// Change one account and publish the edited metadata and password only
    /// after both the account file and the credential store accepted them.
    ///
    /// `None` or an empty password leaves the stored credential untouched.
    /// The bool is `true` when the Riot ID changed, which resets the rank and
    /// invalidates refresh results admitted before the edit.
    pub fn update_account(
        &mut self,
        key: &AccountKey,
        changes: AccountChanges,
    ) -> ManagerResult<(Account, bool)> {
        self.update_account_with(key, changes, &NativePasswords(KEYRING_SERVICE))
    }

    fn update_account_with(
        &mut self,
        key: &AccountKey,
        changes: AccountChanges,
        passwords: &impl PasswordStore,
    ) -> ManagerResult<(Account, bool)> {
        let index = self
            .accounts
            .iter()
            .position(|account| account.key() == *key)
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::NotFound, "account not found")
            })?;
        let mut updated = self.accounts[index].clone();
        let renamed = updated.name != changes.name;
        updated.name = changes.name;
        updated.description = changes.description;
        updated.locale = changes.locale;
        updated.skip_profile_update = changes.skip_profile_update;
        if renamed {
            reset_rank(&mut updated);
        }
        let new_password = changes
            .password
            .as_deref()
            .filter(|password| !password.is_empty());
        if let Some(password) = new_password {
            updated.password = password.to_owned();
        }
        // Prepare the replacement first: a file failure must not drop the
        // credential that is still in use.
        let file = self.prepare_accounts(self.accounts.iter().enumerate().map(
            |(position, account)| if position == index { &updated } else { account },
        ))?;
        let credential_key = format!("{}:{}", key.region, key.account_id);
        let previous_password = match new_password {
            Some(password) => {
                let previous = passwords.snapshot(&credential_key)?;
                if let Err(error) = passwords.set(&credential_key, password) {
                    // Backends must not leave a partial replacement visible
                    // when they report a failed write.
                    passwords.restore(&credential_key, previous)?;
                    return Err(error);
                }
                Some(previous)
            }
            None => None,
        };
        if let Err(error) = self.persist_accounts(file) {
            if let Some(previous) = previous_password {
                passwords.restore(&credential_key, previous)?;
            }
            return Err(error);
        }
        if renamed {
            self.rank_jobs.remove(key);
        }
        self.accounts[index] = updated.clone();
        Ok((updated, renamed))
    }

    pub fn delete_account(&mut self, account_id: &str, region: &str) -> ManagerResult<()> {
        self.delete_account_with(account_id, region, &NativePasswords(KEYRING_SERVICE))
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
        let previous = passwords.snapshot(&key)?;
        passwords.delete(&key)?;
        if let Err(error) = self.persist_accounts(file) {
            passwords.restore(&key, previous)?;
            return Err(error);
        }
        self.rank_jobs.remove(&AccountKey {
            account_id: account_id.to_owned(),
            region: region.to_owned(),
        });
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

    /// Replace the credential and the cached password together. A failed
    /// credential write must leave login, copy and export using the old value.
    pub fn set_password(&mut self, key: &AccountKey, password: &str) -> ManagerResult<()> {
        self.set_password_with(key, password, &NativePasswords(KEYRING_SERVICE))
    }

    fn set_password_with(
        &mut self,
        key: &AccountKey,
        password: &str,
        passwords: &impl PasswordStore,
    ) -> ManagerResult<()> {
        let account = self.account_mut(key).ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::NotFound, "account not found")
        })?;
        let credential_key = format!("{}:{}", account.region, account.account_id);
        passwords.set(&credential_key, password)?;
        account.password = password.to_owned();
        Ok(())
    }

    /// Admit one account to a refresh. Every admission invalidates earlier
    /// tickets for the same account.
    pub fn begin_rank_refresh(&mut self, key: &AccountKey) -> Option<RankJob> {
        let account = self.account(key)?.clone();
        self.rank_generation = self.rank_generation.wrapping_add(1);
        self.rank_jobs.insert(key.clone(), self.rank_generation);
        Some(RankJob {
            account,
            generation: self.rank_generation,
        })
    }

    /// Apply a result only while its ticket is still the newest for the same
    /// key and Riot ID. Deleted, recreated, or renamed accounts reject older
    /// tickets.
    pub fn apply_rank_info(&mut self, job: &RankJob, info: &RankInfo) -> bool {
        let key = job.account.key();
        if self.rank_jobs.get(&key) != Some(&job.generation) {
            return false;
        }
        let Some(account) = self
            .account_mut(&key)
            .filter(|account| account.name == job.account.name)
        else {
            return false;
        };
        info.apply_to(account);
        true
    }

    /// Fetch all ranks with at most four concurrent provider calls.
    pub fn refresh_ranks(&mut self) -> ManagerResult<()> {
        logging::record(Event::RankRefreshStarted, Reason::None);
        let keys: Vec<AccountKey> = self.accounts.iter().map(Account::key).collect();
        let jobs: Vec<RankJob> = keys
            .iter()
            .filter_map(|key| self.begin_rank_refresh(key))
            .collect();
        let results = run_rank_jobs(self.rank_fetcher.clone(), jobs);
        for (job, info) in results {
            self.apply_rank_info(&job, &info);
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
        self.import_accounts_with(json, &NativePasswords(KEYRING_SERVICE))
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
        for (index, item) in data.into_iter().enumerate() {
            let account_id = item.account_id.trim().to_owned();
            if account_id.is_empty() {
                skipped += 1;
                continue;
            }
            let Some(region) = normalize_import_region(&item.region) else {
                return Err(import_error(index, "unsupported region"));
            };
            let locale = match normalized_import_locale(item.locale.as_deref()) {
                Ok(locale) => locale,
                Err(()) => return Err(import_error(index, "invalid locale")),
            };
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
                locale,
                skip_profile_update: item.skip_profile_update,
                rank_provenance: item.rank_provenance,
                tft_provenance: item.tft_provenance,
                // Re-detected at the next sign-in.
                ban: None,
            };
            accounts.push(account);
        }
        let result = self.add_accounts_with(accounts, passwords)?;
        Ok((result.added.len(), skipped + result.skipped_indices.len()))
    }
}

/// Map an exported region (internal code in any case, or a known display
/// label) onto the internal code used everywhere else.
fn normalize_import_region(value: &str) -> Option<String> {
    if value.chars().any(char::is_control) {
        return None;
    }
    let value = value.trim();
    REGION_MAP
        .iter()
        .find(|(label, code)| {
            label.eq_ignore_ascii_case(value) || code.eq_ignore_ascii_case(value)
        })
        .map(|(_, code)| (*code).to_owned())
}

/// Empty values keep Riot's own choice; anything not a known alias is rejected
/// rather than stripped or guessed into a writable value.
fn normalized_import_locale(value: Option<&str>) -> Result<Option<String>, ()> {
    if value.is_some_and(|value| value.chars().any(char::is_control)) {
        return Err(());
    }
    match value.map(str::trim).filter(|value| !value.is_empty()) {
        None => Ok(None),
        Some(value) => locales::normalize(value)
            .map(|locale| Some(locale.to_owned()))
            .ok_or(()),
    }
}

/// Point at the failing import entry without echoing its values.
fn import_error(index: usize, problem: &str) -> Box<dyn Error + Send + Sync> {
    let error = std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        format!("import item {index}: {problem}"),
    );
    logging::record(Event::ImportFailed, Reason::from_error(&error));
    Box::new(error)
}

/// A renamed account no longer matches the Riot ID its ranks were fetched for.
fn reset_rank(account: &mut Account) {
    account.riot_id_not_found = false;
    RankInfo::unranked().apply_to(account);
}

fn run_rank_jobs(
    provider: Arc<dyn RankProvider>,
    jobs: Vec<RankJob>,
) -> Vec<(RankJob, RankInfo)> {
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
            let Some(job) = job else { break };
            // The ticket owns the only account copy; the provider borrows it.
            let result = {
                let account = &job.account;
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    provider.fetch_rank(account)
                }))
                .unwrap_or_else(|_| {
                    logging::record(Event::RankWorkerPanicked, Reason::None);
                    RankInfo::error()
                })
            };
            if sender.send((job, result)).is_err() {
                break;
            }
        }));
    }
    drop(sender);
    let results: Vec<(RankJob, RankInfo)> = receiver.into_iter().collect();
    for worker in workers {
        let _ = worker.join();
    }
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
    #[serde(skip_serializing_if = "Option::is_none")]
    locale: Option<String>,
    skip_profile_update: bool,
    tier: String,
    division: String,
    lp: String,
    level: String,
    reached_last_season: String,
    finished_last_season: String,
    tft: TftRank,
    rank_provenance: Option<RankProvenance>,
    tft_provenance: Option<RankProvenance>,
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
            locale: account.locale.clone(),
            skip_profile_update: account.skip_profile_update,
            tier: account.tier.clone(),
            division: account.division.clone(),
            lp: account.lp.clone(),
            level: account.level.clone(),
            reached_last_season: account.reached_last_season.clone(),
            finished_last_season: account.finished_last_season.clone(),
            tft: account.tft.clone(),
            rank_provenance: account.rank_provenance.clone(),
            tft_provenance: account.tft_provenance.clone(),
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
    locale: Option<String>,
    #[serde(default)]
    skip_profile_update: bool,
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
    #[serde(default)]
    rank_provenance: Option<RankProvenance>,
    #[serde(default)]
    tft_provenance: Option<RankProvenance>,
}

#[cfg(test)]
mod tests {
    use super::*;

    // The native store is shared outside the process, unlike TestPasswords.
    // Keep fixture writes and cleanup within one admitted credential session.
    #[cfg(windows)]
    static NATIVE_CREDENTIALS: Mutex<()> = Mutex::new(());

    #[test]
    fn export_round_trips_account_settings_and_rank_provenance() {
        let directory = tempfile::tempdir().unwrap();
        let mut manager = test_manager(directory.path().join("accounts.json"));
        let account = Account {
            locale: Some("zh_CN".into()),
            skip_profile_update: true,
            rank_provenance: Some(RankProvenance {
                source: "OP.GG".into(),
                source_updated_at: Some(42),
                ..Default::default()
            }),
            tft_provenance: Some(RankProvenance::default()),
            ..test_account("exported")
        };
        manager.accounts.push(account.clone());
        let json = manager.export_accounts().unwrap();
        let mut restored = test_manager(directory.path().join("restored.json"));
        let passwords = TestPasswords::default();
        assert_eq!(restored.import_accounts_with(&json, &passwords).unwrap(), (1, 0));
        let imported = &restored.accounts[0];
        assert_eq!(imported.locale, account.locale);
        assert_eq!(imported.skip_profile_update, account.skip_profile_update);
        assert_eq!(imported.rank_provenance, account.rank_provenance);
        assert_eq!(imported.tft_provenance, account.tft_provenance);
    }

    #[test]
    fn export_round_trips_locale_and_skip_profile_update() {
        let directory = tempfile::tempdir().unwrap();
        let mut source = test_manager(directory.path().join("source.json"));
        source.accounts.push(Account {
            account_id: "synthetic-backup-account".into(),
            region: "euw".into(),
            // A cached synthetic value avoids even reading the native store.
            password: "synthetic-backup-secret".into(),
            locale: Some("zh_MY".into()),
            skip_profile_update: true,
            ..Account::default()
        });
        let mut exported: serde_json::Value =
            serde_json::from_str(&source.export_accounts().unwrap()).unwrap();
        assert_eq!(exported[0]["locale"], "zh_MY");
        assert_eq!(exported[0]["skip_profile_update"], true);
        // Passwordless import needs no credential store and tests the real
        // import path without accessing any native target.
        exported[0]["password"] = serde_json::Value::String(String::new());
        let mut restored = test_manager(directory.path().join("restored.json"));
        assert_eq!(restored.import_accounts(&exported.to_string()).unwrap(), (1, 0));
        assert_eq!(restored.accounts[0].locale.as_deref(), Some("zh_MY"));
        assert!(restored.accounts[0].skip_profile_update);

        // Backups from before these fields existed keep the defaults.
        assert_eq!(restored.import_accounts(
            r#"[{"account_id":"old","region":"euw"}]"#
        ).unwrap(), (1, 0));
        let old = restored.accounts.iter().find(|account| account.account_id == "old").unwrap();
        assert!(old.locale.is_none());
        assert!(!old.skip_profile_update);
    }

    #[test]
    fn a_missing_accounts_file_loads_empty_and_stays_writable() {
        let directory = tempfile::tempdir().unwrap();
        let mut manager = test_manager(directory.path().join("accounts.json"));

        manager.load_accounts().unwrap();
        assert!(manager.accounts.is_empty());

        let passwords = TestPasswords::default();
        let result = manager
            .add_accounts_with(vec![test_account("new")], &passwords)
            .unwrap();
        assert_eq!(result.added.len(), 1);
        assert!(manager.accounts_file.exists());
    }

    #[test]
    fn corrupt_accounts_are_backed_up_without_overwriting_and_block_writes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("accounts.json");
        let mut manager = test_manager(&path);
        manager.accounts.push(test_account("kept"));
        manager.save_accounts().unwrap();
        let original_memory = manager.accounts.clone();

        let earlier_backup = directory.path().join("accounts.json.corrupt");
        std::fs::write(&earlier_backup, "earlier backup").unwrap();
        let corrupt = br#"[{"account_id": "broken""#;
        std::fs::write(&path, corrupt).unwrap();

        assert!(manager.load_accounts().is_err());
        // The previous in-memory list survives, and writes cannot replace a
        // file that was never understood.
        assert_eq!(manager.accounts, original_memory);
        assert_eq!(std::fs::read(&earlier_backup).unwrap(), b"earlier backup");
        let unique_backup = directory.path().join("accounts.json.corrupt.1");
        assert_eq!(std::fs::read(&unique_backup).unwrap(), corrupt);
        assert!(manager.save_accounts().is_err());
        assert_eq!(std::fs::read(&path).unwrap(), corrupt);
        let passwords = TestPasswords::default();
        passwords.set("euw:kept", "stored-password").unwrap();
        let key = manager.accounts[0].key();
        assert!(manager.add_accounts_with(vec![test_account("new")], &passwords).is_err());
        assert!(manager.delete_account_with("kept", "euw", &passwords).is_err());
        assert!(manager.update_account_with(&key, AccountChanges {
            name: "Changed#EUW".into(),
            description: String::new(),
            locale: None,
            skip_profile_update: false,
            password: Some("different-password".into()),
        }, &passwords).is_err());
        assert!(manager.import_accounts_with(
            r#"[{"account_id":"new","region":"euw"}]"#, &passwords
        ).is_err());
        assert_eq!(passwords.get("euw:kept").unwrap().as_deref(), Some("stored-password"));
        assert_eq!(manager.accounts, original_memory);
        assert_eq!(std::fs::read(&path).unwrap(), corrupt);

        // Invalid UTF-8 is readable corruption too: preserve exact bytes,
        // with another unique backup rather than treating it as a read error.
        let non_utf8 = [0xff, 0xfe, b'['];
        std::fs::write(&path, non_utf8).unwrap();
        assert!(manager.load_accounts().is_err());
        assert_eq!(std::fs::read(directory.path().join("accounts.json.corrupt.2")).unwrap(), non_utf8);
        assert_eq!(std::fs::read(&unique_backup).unwrap(), corrupt);
        assert_eq!(manager.accounts, original_memory);

        // Only an explicit successful reload recovers the manager.
        std::fs::write(&path, "[]").unwrap();
        manager.load_accounts().unwrap();
        assert!(manager.accounts.is_empty());
        assert!(manager.save_accounts().is_ok());
    }

    #[test]
    fn an_unreadable_store_is_not_reported_as_backed_up_or_overwritten() {
        let directory = tempfile::tempdir().unwrap();
        let blocked = directory.path().join("accounts.json");
        std::fs::create_dir(&blocked).unwrap();
        let mut manager = test_manager(&blocked);
        manager.accounts.push(test_account("kept"));
        let original_memory = manager.accounts.clone();

        assert!(manager.load_accounts().is_err());
        assert_eq!(manager.accounts, original_memory);
        // A read failure creates no backup file and leaves the old memory.
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
        assert!(manager
            .add_accounts_with(vec![test_account("new")], &TestPasswords::default())
            .is_err());
        assert_eq!(manager.accounts, original_memory);
    }

    #[test]
    fn update_account_commits_metadata_and_password_together() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("accounts.json");
        let mut manager = test_manager(&path);
        manager.accounts.push(Account {
            account_id: "user".into(),
            name: "Old#EUW".into(),
            region: "euw".into(),
            password: "old-password".into(),
            tier: "Diamond".into(),
            ..Account::default()
        });
        manager.save_accounts().unwrap();
        let passwords = TestPasswords::default();
        passwords.set("euw:user", "old-password").unwrap();
        let key = manager.accounts[0].key();
        let ticket = manager.begin_rank_refresh(&key).unwrap();

        let (updated, renamed) = manager
            .update_account_with(
                &key,
                AccountChanges {
                    name: "New#EUW".into(),
                    description: "note".into(),
                    locale: Some("zh_MY".into()),
                    skip_profile_update: true,
                    password: Some("new-password".into()),
                },
                &passwords,
            )
            .unwrap();

        assert!(renamed);
        assert_eq!(updated.name, "New#EUW");
        assert_eq!(updated.password, "new-password");
        assert_eq!(updated.tier, "Unranked");
        assert_eq!(manager.accounts[0], updated);
        assert_eq!(
            passwords.get("euw:user").unwrap().as_deref(),
            Some("new-password")
        );
        // The rename invalidated the ticket admitted for the old Riot ID.
        assert!(!manager.apply_rank_info(
            &ticket,
            &RankInfo {
                tier: "Gold".into(),
                ..RankInfo::default()
            }
        ));

        let saved: Vec<Account> = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved[0].name, "New#EUW");
        assert_eq!(saved[0].locale.as_deref(), Some("zh_MY"));
        assert!(saved[0].skip_profile_update);
        assert!(saved[0].password.is_empty());

        // Login, copy and export all read the same committed password.
        let exported: Vec<serde_json::Value> =
            serde_json::from_str(&manager.export_accounts().unwrap()).unwrap();
        assert_eq!(exported[0]["password"], "new-password");
        assert_eq!(exported[0]["locale"], "zh_MY");
        assert_eq!(exported[0]["skip_profile_update"], true);
    }

    #[test]
    fn update_account_without_a_password_keeps_the_stored_credential() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("accounts.json");
        let mut manager = test_manager(&path);
        manager.accounts.push(Account {
            account_id: "user".into(),
            name: "Same#EUW".into(),
            region: "euw".into(),
            password: "kept-password".into(),
            ..Account::default()
        });
        manager.save_accounts().unwrap();
        let passwords = TestPasswords::default();
        passwords.set("euw:user", "kept-password").unwrap();
        let key = manager.accounts[0].key();

        for password in [None, Some(String::new())] {
            let (updated, renamed) = manager
                .update_account_with(
                    &key,
                    AccountChanges {
                        name: "Same#EUW".into(),
                        description: "note".into(),
                        locale: None,
                        skip_profile_update: false,
                        password,
                    },
                    &passwords,
                )
                .unwrap();
            assert!(!renamed);
            assert_eq!(updated.password, "kept-password");
            assert_eq!(
                passwords.get("euw:user").unwrap().as_deref(),
                Some("kept-password")
            );
        }
    }

    #[test]
    fn failed_update_restores_memory_file_and_credential() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("accounts.json");
        let mut manager = test_manager(&path);
        manager.accounts.push(Account {
            account_id: "user".into(),
            name: "Old#EUW".into(),
            region: "euw".into(),
            password: "old-password".into(),
            ..Account::default()
        });
        manager.save_accounts().unwrap();
        let original = manager.accounts.clone();
        let original_file = std::fs::read(&path).unwrap();
        let key = manager.accounts[0].key();
        let changes = |password: Option<&str>| AccountChanges {
            name: "Renamed#EUW".into(),
            description: String::new(),
            locale: None,
            skip_profile_update: false,
            password: password.map(str::to_owned),
        };

        let mut passwords = TestPasswords::default();
        passwords.set("euw:user", "old-password").unwrap();
        let blocked = directory.path().join("blocked.json");
        std::fs::create_dir(&blocked).unwrap();
        manager.accounts_file = blocked;
        assert!(manager
            .update_account_with(&key, changes(Some("new-password")), &passwords)
            .is_err());
        assert_eq!(manager.accounts, original);
        assert_eq!(
            passwords.get("euw:user").unwrap().as_deref(),
            Some("old-password")
        );

        manager.accounts_file = path;
        passwords.fail_set = Some("euw:user".into());
        assert!(manager
            .update_account_with(&key, changes(Some("new-password")), &passwords)
            .is_err());
        assert_eq!(manager.accounts, original);
        assert_eq!(std::fs::read(&manager.accounts_file).unwrap(), original_file);
        assert_eq!(
            passwords.get("euw:user").unwrap().as_deref(),
            Some("old-password")
        );

        // Even a backend that changed the target before returning an error
        // must leave the old file, memory and password after rollback.
        passwords.fail_set = None;
        *passwords.fail_after_set_once.borrow_mut() = Some("euw:user".into());
        assert!(manager
            .update_account_with(&key, changes(Some("new-password")), &passwords)
            .is_err());
        assert_eq!(manager.accounts, original);
        assert_eq!(std::fs::read(&manager.accounts_file).unwrap(), original_file);
        assert_eq!(passwords.get("euw:user").unwrap().as_deref(), Some("old-password"));
    }

    #[test]
    fn deleting_and_recreating_an_account_invalidates_old_tickets() {
        let directory = tempfile::tempdir().unwrap();
        let mut manager = test_manager(directory.path().join("accounts.json"));
        manager.accounts.push(test_account("user"));
        manager.save_accounts().unwrap();
        let passwords = TestPasswords::default();
        let key = manager.accounts[0].key();
        let ticket = manager.begin_rank_refresh(&key).unwrap();
        let info = RankInfo {
            tier: "Gold".into(),
            ..RankInfo::default()
        };

        manager
            .delete_account_with("user", "euw", &passwords)
            .unwrap();
        assert!(!manager.apply_rank_info(&ticket, &info));

        manager
            .add_accounts_with(vec![test_account("user")], &passwords)
            .unwrap();
        assert!(!manager.apply_rank_info(&ticket, &info));
        let ticket = manager.begin_rank_refresh(&key).unwrap();
        assert!(manager.apply_rank_info(&ticket, &info));
    }

    #[test]
    fn import_normalizes_region_and_locale() {
        let directory = tempfile::tempdir().unwrap();
        let mut manager = test_manager(directory.path().join("accounts.json"));
        let passwords = TestPasswords::default();
        let json = r#"[
            {"account_id":"one","region":" EUW ","locale":" zh-my "},
            {"account_id":"two","region":"EUNE","locale":"ZH_MY","skip_profile_update":true},
            {"account_id":"three","region":"na","locale":""}
        ]"#;

        assert_eq!(
            manager.import_accounts_with(json, &passwords).unwrap(),
            (3, 0)
        );
        let one = manager
            .accounts
            .iter()
            .find(|account| account.account_id == "one")
            .unwrap();
        assert_eq!(one.region, "euw");
        assert_eq!(one.locale.as_deref(), Some("zh_MY"));
        assert!(!one.skip_profile_update);
        let two = manager
            .accounts
            .iter()
            .find(|account| account.account_id == "two")
            .unwrap();
        assert_eq!(two.region, "eune");
        assert_eq!(two.locale.as_deref(), Some("zh_MY"));
        assert!(two.skip_profile_update);
        let three = manager
            .accounts
            .iter()
            .find(|account| account.account_id == "three")
            .unwrap();
        assert_eq!(three.region, "na");
        assert!(three.locale.is_none());
    }

    #[test]
    fn import_rejects_unknown_region_and_locale_without_writing() {
        let directory = tempfile::tempdir().unwrap();
        let mut manager = test_manager(directory.path().join("accounts.json"));
        let passwords = TestPasswords::default();

        let error = manager
            .import_accounts_with(r#"[{"account_id":"bad","region":"NA1"}]"#, &passwords)
            .unwrap_err();
        assert!(error.to_string().contains("item 0"), "{error}");
        assert!(manager.accounts.is_empty());
        assert!(!manager.accounts_file.exists());

        for locale in ["xx_YY", "zh_MY\"\n  injected: true", "en_US\n", "\nen_US", "\ten_US"] {
            let json = format!(
                r#"[{{"account_id":"bad","region":"euw","locale":{}}}]"#,
                serde_json::to_string(locale).unwrap()
            );
            let error = manager
                .import_accounts_with(&json, &passwords)
                .unwrap_err();
            assert!(error.to_string().contains("item 0"), "{error}");
            assert!(manager.accounts.is_empty());
            assert!(!manager.accounts_file.exists());
        }
        for region in ["euw\n", "\neuw", "euw\t", "\0EUW"] {
            let json = serde_json::json!([{
                "account_id": "bad",
                "region": region,
                "locale": "en_US"
            }]).to_string();
            assert!(manager.import_accounts_with(&json, &passwords).is_err());
            assert!(manager.accounts.is_empty());
            assert!(!manager.accounts_file.exists());
        }
    }
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    #[derive(Default)]
    struct TestPasswords {
        values: RefCell<HashMap<String, String>>,
        fail_set: Option<String>,
        fail_after_set_once: RefCell<Option<String>>,
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
            let fail_after_set = self.fail_after_set_once.borrow().as_deref() == Some(key);
            if fail_after_set {
                self.fail_after_set_once.borrow_mut().take();
                return Err(std::io::Error::other("partial credential write failed").into());
            }
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

    #[test]
    fn password_edits_update_the_cache_only_after_the_credential_write_succeeds() {
        let directory = tempfile::tempdir().unwrap();
        let mut manager = test_manager(directory.path().join("accounts.json"));
        let mut passwords = TestPasswords::default();
        let edited = test_account("edited");
        let key = edited.key();
        let other = test_account("other");
        manager
            .add_accounts_with(vec![edited, other.clone()], &passwords)
            .unwrap();
        manager.set_password_with(&key, "new-password", &passwords).unwrap();
        assert_eq!(manager.account(&key).unwrap().password, "new-password");
        let credential_key = format!("{}:{}", key.region, key.account_id);
        assert_eq!(passwords.get(&credential_key).unwrap().as_deref(), Some("new-password"));
        let exported: Vec<ImportAccount> =
            serde_json::from_str(&manager.export_accounts().unwrap()).unwrap();
        let exported = exported.iter().find(|account| account.account_id == key.account_id).unwrap();
        assert_eq!(exported.password, "new-password");
        assert_eq!(manager.account(&other.key()).unwrap(), &other);

        passwords.fail_set = Some(credential_key.clone());
        assert!(manager.set_password_with(&key, "refused-password", &passwords).is_err());
        assert_eq!(manager.account(&key).unwrap().password, "new-password");
        assert_eq!(passwords.get(&credential_key).unwrap().as_deref(), Some("new-password"));
        manager.save_accounts().unwrap();
        let saved = std::fs::read_to_string(&manager.accounts_file).unwrap();
        assert!(!saved.contains("new-password"));
        assert!(!saved.contains("refused-password"));
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
                ..RankInfo::default()
            }
        }
    }

    #[test]
    fn a_result_for_an_old_riot_id_cannot_overwrite_the_edited_account() {
        let directory = tempfile::tempdir().unwrap();
        let mut manager = test_manager(directory.path().join("accounts.json"));
        manager.accounts.push(Account {
            account_id: "user".into(),
            name: "Old Name#TAG".into(),
            region: "euw".into(),
            tier: "Unranked".into(),
            ..Account::default()
        });
        let key = manager.accounts[0].key();
        let ticket = manager.begin_rank_refresh(&key).unwrap();
        let info = RankInfo {
            tier: "Diamond".into(),
            ..RankInfo::default()
        };

        manager.accounts[0].name = "New Name#TAG".into();
        assert!(!manager.apply_rank_info(&ticket, &info));
        assert_eq!(manager.accounts[0].tier, "Unranked");

        // A newer admission for the same key replaces the old ticket.
        let stale = manager.begin_rank_refresh(&key).unwrap();
        let current = manager.begin_rank_refresh(&key).unwrap();
        assert!(!manager.apply_rank_info(&stale, &info));
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

    #[cfg(windows)]
    #[test]
    fn native_rollback_does_not_modify_another_accounts_credential() {
        let _credentials = NATIVE_CREDENTIALS.lock().unwrap();
        use std::os::windows::fs::OpenOptionsExt;

        // Use an isolated service, and clean up its dummy entries even if an
        // assertion fails. Never access the application's credential targets.
        struct Cleanup(Vec<keyring::Entry>);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                for entry in &self.0 {
                    let _ = entry.delete_credential();
                }
            }
        }

        let directory = tempfile::tempdir().unwrap();
        let service = format!(
            "LeagueAccounts-test-{}-{}",
            std::process::id(),
            directory.path().file_name().unwrap().to_string_lossy()
        );
        let entries = Cleanup(vec![
            keyring::Entry::new_with_target(&service, &service, "euw:a").unwrap(),
            keyring::Entry::new_with_target(&format!("euw:a@{service}"), &service, "euw:a")
                .unwrap(),
            keyring::Entry::new_with_target(&format!("euw:b@{service}"), &service, "euw:b")
                .unwrap(),
        ]);
        let legacy = &entries.0[0];
        let primary_a = &entries.0[1];
        let primary_b = &entries.0[2];
        legacy.set_password("stale-a-password").unwrap();
        primary_a.set_password("current-a-password").unwrap();
        primary_b.set_password("current-b-password").unwrap();

        let path = directory.path().join("accounts.json");
        let mut manager = test_manager(&path);
        manager.accounts = vec![test_account("a"), test_account("b")];
        manager.save_accounts().unwrap();
        let original_accounts = manager.accounts.clone();
        let original_file = std::fs::read(&path).unwrap();
        let _locked = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(&path)
            .unwrap();
        let passwords = NativePasswords(&service);

        assert!(manager.delete_account_with("b", "euw", &passwords).is_err());
        assert_eq!(primary_a.get_password().unwrap(), "current-a-password");
        assert_eq!(primary_b.get_password().unwrap(), "current-b-password");
        assert_eq!(legacy.get_password().unwrap(), "stale-a-password");
        assert_eq!(manager.accounts, original_accounts);
        assert_eq!(std::fs::read(&path).unwrap(), original_file);

        // A failed file commit after deleting both copies restores both the
        // primary and the matching legacy secret, not just the primary.
        assert!(manager.delete_account_with("a", "euw", &passwords).is_err());
        assert_eq!(primary_a.get_password().unwrap(), "current-a-password");
        assert_eq!(legacy.get_password().unwrap(), "stale-a-password");
        assert_eq!(primary_b.get_password().unwrap(), "current-b-password");
        assert_eq!(manager.accounts, original_accounts);
        assert_eq!(std::fs::read(&path).unwrap(), original_file);

        // Deleting the account clears the matching stale legacy entry too,
        // while another account's primary credential is untouched.
        passwords.delete("euw:a").unwrap();
        assert!(passwords.get("euw:a").unwrap().is_none());
        assert!(matches!(legacy.get_password(), Err(keyring::Error::NoEntry)));
        assert_eq!(primary_b.get_password().unwrap(), "current-b-password");
    }

    #[cfg(windows)]
    #[test]
    fn loading_migrates_legacy_only_passwords_without_changing_other_credentials() {
        let _credentials = NATIVE_CREDENTIALS.lock().unwrap();
        // Isolate every target, including the shared legacy entry, from the
        // user's credentials and from the native rollback regression above.
        struct Cleanup(Vec<keyring::Entry>);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                for entry in &self.0 {
                    let _ = entry.delete_credential();
                }
            }
        }

        let directory = tempfile::tempdir().unwrap();
        let service = format!(
            "LeagueAccounts-migration-test-{}-{}",
            std::process::id(),
            directory.path().file_name().unwrap().to_string_lossy()
        );
        let entries = Cleanup(vec![
            keyring::Entry::new_with_target(&service, &service, "euw:legacy").unwrap(),
            keyring::Entry::new_with_target(
                &format!("euw:legacy@{service}"),
                &service,
                "euw:legacy",
            )
            .unwrap(),
            keyring::Entry::new_with_target(
                &format!("euw:current@{service}"),
                &service,
                "euw:current",
            )
            .unwrap(),
            keyring::Entry::new_with_target(
                &format!("euw:missing@{service}"),
                &service,
                "euw:missing",
            )
            .unwrap(),
        ]);
        let legacy = &entries.0[0];
        let migrated = &entries.0[1];
        let current = &entries.0[2];
        legacy.set_password("legacy-password").unwrap();
        current.set_password("current-password").unwrap();

        let path = directory.path().join("accounts.json");
        let mut manager = test_manager(&path);
        manager.accounts = vec![
            test_account("missing"),
            test_account("current"),
            test_account("legacy"),
        ];
        manager.save_accounts().unwrap();
        let original_file = std::fs::read(&path).unwrap();
        // Transactional reads must not consult or migrate the shared entry.
        let passwords = NativePasswords(&service);
        assert!(passwords.get("euw:legacy").unwrap().is_none());

        manager.load_accounts_with_service(&service).unwrap();
        assert_eq!(manager.accounts[0].account_id, "missing");
        assert!(manager.accounts[0].password.is_empty());
        assert_eq!(manager.accounts[1].password, "current-password");
        assert_eq!(manager.accounts[2].password, "legacy-password");
        assert_eq!(migrated.get_password().unwrap(), "legacy-password");
        assert_eq!(current.get_password().unwrap(), "current-password");
        assert!(passwords.get("euw:missing").unwrap().is_none());
        // The shared legacy entry is removed only after the copy succeeded.
        assert!(matches!(legacy.get_password(), Err(keyring::Error::NoEntry)));
        assert_eq!(std::fs::read(&path).unwrap(), original_file);

        // A primary entry is authoritative even when a stale legacy copy
        // reappears; loading clears the stale copy. The loaded password must
        // also remain available for exports.
        legacy.set_password("legacy-password").unwrap();
        migrated.set_password("updated-password").unwrap();
        manager
            .accounts
            .retain(|account| account.account_id != "missing");
        manager.save_accounts().unwrap();
        manager.load_accounts_with_service(&service).unwrap();
        assert_eq!(manager.accounts[1].password, "updated-password");
        let exported: serde_json::Value =
            serde_json::from_str(&manager.export_accounts().unwrap()).unwrap();
        assert_eq!(exported[1]["password"], "updated-password");
        assert!(matches!(legacy.get_password(), Err(keyring::Error::NoEntry)));
        assert_eq!(migrated.get_password().unwrap(), "updated-password");

        // Even an explicitly empty primary is not a missing entry.
        legacy.set_password("stale-again").unwrap();
        migrated.set_password("").unwrap();
        manager.load_accounts_with_service(&service).unwrap();
        assert!(manager.accounts[1].password.is_empty());
        assert_eq!(migrated.get_password().unwrap(), "");
        assert!(matches!(legacy.get_password(), Err(keyring::Error::NoEntry)));
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
