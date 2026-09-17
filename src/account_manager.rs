use crate::credentials;
use crate::models::{Account, AccountKey, RankInfo};
use crate::rank_fetcher::RankProvider;
use crate::utils::{accounts_file, region_display, sort_accounts};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::error::Error;
use std::path::PathBuf;
use std::sync::{mpsc, Arc, Mutex};
use std::thread;

pub const KEYRING_SERVICE: &str = credentials::SERVICE;

pub type ManagerResult<T> = Result<T, Box<dyn Error + Send + Sync>>;

pub struct AccountManager {
    pub accounts: Vec<Account>,
    pub accounts_file: PathBuf,
    pub rank_fetcher: Arc<dyn RankProvider>,
}

impl AccountManager {
    pub fn new(rank_fetcher: Arc<dyn RankProvider>) -> ManagerResult<Self> {
        Ok(Self {
            accounts: Vec::new(),
            accounts_file: accounts_file()?,
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
        let contents = std::fs::read_to_string(&self.accounts_file)?;
        let mut loaded: Vec<Account> = serde_json::from_str(&contents)?;
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
        if let Some(parent) = self.accounts_file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(&self.accounts)?;
        std::fs::write(&self.accounts_file, json)?;
        Ok(())
    }

    pub fn add_account(&mut self, account: Account) -> ManagerResult<()> {
        self.accounts.push(account);
        sort_accounts(&mut self.accounts);
        self.save_accounts()
    }

    pub fn delete_account(&mut self, account_id: &str, region: &str) -> ManagerResult<()> {
        self.accounts
            .retain(|account| !(account.account_id == account_id && account.region == region));
        let _ = credentials::delete_password(KEYRING_SERVICE, &format!("{region}:{account_id}"));
        self.save_accounts()
    }

    pub fn account(&self, key: &AccountKey) -> Option<&Account> {
        self.accounts.iter().find(|account| account.key() == *key)
    }

    pub fn account_mut(&mut self, key: &AccountKey) -> Option<&mut Account> {
        self.accounts
            .iter_mut()
            .find(|account| account.key() == *key)
    }

    pub fn apply_rank_info(&mut self, key: &AccountKey, info: &RankInfo) {
        if let Some(account) = self.account_mut(key) {
            info.apply_to(account);
        }
    }

    /// Fetch all ranks with at most four concurrent provider calls.
    pub fn refresh_ranks(&mut self) -> ManagerResult<()> {
        let jobs: Vec<(usize, Account)> = self.accounts.iter().cloned().enumerate().collect();
        let results = run_rank_jobs(self.rank_fetcher.clone(), jobs);
        for (index, info) in results {
            if let Some(account) = self.accounts.get_mut(index) {
                info.apply_to(account);
            }
        }
        sort_accounts(&mut self.accounts);
        self.save_accounts()
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
        Ok(serde_json::to_string_pretty(&export)?)
    }

    /// Import a JSON array. Returns `(added, skipped)` like the original app.
    pub fn import_accounts(&mut self, json: &str) -> ManagerResult<(usize, usize)> {
        let data: Vec<ImportAccount> = serde_json::from_str(json)?;
        let mut added = 0;
        let mut skipped = 0;
        for item in data {
            let account_id = item.account_id.trim().to_owned();
            let region = item.region.trim().to_owned();
            if account_id.is_empty() || region.is_empty() {
                skipped += 1;
                continue;
            }
            if self.accounts.iter().any(|account| {
                account.account_id.eq_ignore_ascii_case(&account_id) && account.region == region
            }) {
                skipped += 1;
                continue;
            }
            let account = Account {
                account_id: account_id.clone(),
                name: item.name,
                region: region.clone(),
                region_display: if item.region_display.is_empty() {
                    region_display(&region)
                } else {
                    item.region_display
                },
                password: item.password.clone(),
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
            };
            if !account.password.is_empty() {
                let _ = credentials::set_password(
                    KEYRING_SERVICE,
                    &format!("{region}:{account_id}"),
                    &account.password,
                );
            }
            self.accounts.push(account);
            added += 1;
        }
        sort_accounts(&mut self.accounts);
        if added > 0 {
            self.save_accounts()?;
        }
        Ok((added, skipped))
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
            .unwrap_or_else(|_| RankInfo::error());
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
}

impl From<&Account> for ExportAccount {
    fn from(account: &Account) -> Self {
        Self {
            account_id: account.account_id.clone(),
            name: account.name.clone(),
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

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
                tier: "Gold".into(),
                division: "II".into(),
                lp: "50".into(),
                level: "100".into(),
                reached_last_season: "Platinum IV".into(),
                finished_last_season: "Gold I".into(),
            }
        }
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
        let path = tempfile::tempdir().unwrap().path().join("accounts.json");
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
}
