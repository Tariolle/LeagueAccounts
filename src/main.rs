#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use eframe::egui::{
    self, Color32, Context, Key, Modifiers, RichText, TextureHandle, TextureOptions,
};
use leagueaccounts::account_manager::KEYRING_SERVICE;
use leagueaccounts::credentials;
use leagueaccounts::models::{Account, AccountKey, RankInfo};
use leagueaccounts::rank_fetcher::{RankFetcher, RankProvider};
use leagueaccounts::utils::{region_from_display, sort_accounts, REGION_MAP, TIER_ORDER};
use leagueaccounts::AccountManager;
use std::collections::VecDeque;
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

mod shortcuts;

const RANKS_IMAGE: &[u8] = include_bytes!("../assets/ranks_compatibilities.webp");
const TABLE_HEIGHT: f32 = 270.0;

#[derive(Clone)]
struct RankUpdate {
    key: AccountKey,
    info: RankInfo,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum EditField {
    Name,
    Description,
}

struct EditState {
    key: AccountKey,
    field: EditField,
    value: String,
}

struct LeagueAccountsApp {
    manager: AccountManager,
    search: String,
    selected: Option<AccountKey>,
    last_copied: Option<AccountKey>,
    copy_counter: u8,
    add_account_id: String,
    add_name: String,
    add_region: String,
    add_password: String,
    add_description: String,
    multi_add: String,
    friend_tier: String,
    friend_division: String,
    status: String,
    rank_updates: Option<mpsc::Receiver<RankUpdate>>,
    rank_sender: Option<mpsc::Sender<RankUpdate>>,
    rank_jobs_pending: usize,
    edit_state: Option<EditState>,
    show_help: bool,
    show_ranks: bool,
    ranks_texture: Option<TextureHandle>,
    #[cfg(windows)]
    native_auto_type: Option<shortcuts::NativeAutoType>,
}

impl LeagueAccountsApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let provider: Arc<dyn RankProvider> = Arc::new(RankFetcher::new());
        let mut manager =
            AccountManager::new(provider).expect("unable to locate the application data directory");
        let status = match manager.load_accounts() {
            Ok(()) => format!("{} account(s) loaded", manager.accounts.len()),
            Err(error) => format!("Could not load accounts: {error}"),
        };
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        #[cfg(windows)]
        {
            let mut app = Self::with_manager(manager, status);
            if let Some(window) = cc.winit_window() {
                let window = Arc::clone(window);
                match shortcuts::NativeAutoType::install(cc.egui_ctx.clone(), move || {
                    window.has_focus()
                }) {
                    Ok(hook) => app.native_auto_type = Some(hook),
                    Err(error) => {
                        app.status = format!("{}; Auto-type shortcut unavailable: {error}", app.status);
                    }
                }
            }
            app
        }
        #[cfg(not(windows))]
        {
            Self::with_manager(manager, status)
        }
    }

    fn with_manager(manager: AccountManager, status: String) -> Self {
        Self {
            manager,
            search: String::new(),
            selected: None,
            last_copied: None,
            copy_counter: 0,
            add_account_id: String::new(),
            add_name: String::new(),
            add_region: "EUW".to_owned(),
            add_password: String::new(),
            add_description: String::new(),
            multi_add: String::new(),
            friend_tier: "Show All".to_owned(),
            friend_division: "I".to_owned(),
            status,
            rank_updates: None,
            rank_sender: None,
            rank_jobs_pending: 0,
            edit_state: None,
            show_help: false,
            show_ranks: false,
            ranks_texture: None,
            #[cfg(windows)]
            native_auto_type: None,
        }
    }

    fn selected_account(&self) -> Option<Account> {
        self.selected
            .as_ref()
            .and_then(|key| self.manager.account(key).cloned())
    }

    fn visible_accounts(&self) -> Vec<Account> {
        let search = self.search.trim().to_ascii_lowercase();
        self.manager
            .accounts
            .iter()
            .filter(|account| {
                search.is_empty()
                    || account.name.to_ascii_lowercase().contains(&search)
                    || account.account_id.to_ascii_lowercase().contains(&search)
            })
            .filter(|account| {
                self.friend_tier == "Show All"
                    || self.can_play_with(account, &self.friend_tier, &self.friend_division)
            })
            .cloned()
            .collect()
    }

    fn add_account(&mut self) {
        let account_id = self.add_account_id.trim().to_owned();
        let name = self.add_name.trim().to_owned();
        let region_display_name = self.add_region.trim().to_owned();
        let password = self.add_password.trim().to_owned();
        let description = self.add_description.trim().to_owned();
        if account_id.is_empty() || name.is_empty() || password.is_empty() {
            self.status = "Please fill in Account ID, Summoner Name, and Password.".to_owned();
            return;
        }
        let Some(region) = region_from_display(&region_display_name) else {
            self.status = "Invalid region selected.".to_owned();
            return;
        };
        if self.manager.accounts.iter().any(|account| {
            account.account_id.eq_ignore_ascii_case(&account_id) && account.region == region
        }) {
            self.status = "This account is already added.".to_owned();
            return;
        }
        if let Err(error) = credentials::set_password(
            KEYRING_SERVICE,
            &format!("{region}:{account_id}"),
            &password,
        ) {
            self.status = format!("Could not save password: {error}");
            return;
        }
        let account = Account {
            account_id,
            name,
            region: region.to_owned(),
            region_display: region_display_name,
            password,
            description,
            tier: "Unranked".to_owned(),
            reached_last_season: "N/A".to_owned(),
            finished_last_season: "N/A".to_owned(),
            ..Account::default()
        };
        let key = account.key();
        match self.manager.add_account(account.clone()) {
            Ok(()) => {
                self.start_rank_jobs(vec![account]);
                self.selected = Some(key);
                self.add_account_id.clear();
                self.add_name.clear();
                self.add_password.clear();
                self.add_description.clear();
                self.status = "Account added; fetching rank information…".to_owned();
            }
            Err(error) => self.status = format!("Could not save account: {error}"),
        }
    }

    fn multi_add_accounts(&mut self) {
        let region_display_name = self.add_region.trim().to_owned();
        let Some(region) = region_from_display(&region_display_name) else {
            self.status = "Invalid region selected.".to_owned();
            return;
        };
        let input = self.multi_add.trim().to_owned();
        if input.is_empty() {
            self.status = "Please enter account data in the multi-add field.".to_owned();
            return;
        }
        let mut added_accounts = Vec::new();
        let mut added = 0;
        let mut skipped = 0;
        for line in input.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let parts: Vec<&str> = if line.contains("---") {
                line.split("---").collect()
            } else {
                line.split("--").collect()
            };
            if parts.len() != 3 {
                skipped += 1;
                continue;
            }
            let account_id = parts[0].trim();
            let name = parts[1].trim();
            let password = parts[2].trim();
            if account_id.is_empty()
                || name.is_empty()
                || password.is_empty()
                || self.manager.accounts.iter().any(|account| {
                    account.account_id.eq_ignore_ascii_case(account_id) && account.region == region
                })
                || added_accounts.iter().any(|account: &Account| {
                    account.account_id.eq_ignore_ascii_case(account_id) && account.region == region
                })
            {
                skipped += 1;
                continue;
            }
            if credentials::set_password(
                KEYRING_SERVICE,
                &format!("{region}:{account_id}"),
                password,
            )
            .is_err()
            {
                skipped += 1;
                continue;
            }
            let account = Account {
                account_id: account_id.to_owned(),
                name: name.to_owned(),
                region: region.to_owned(),
                region_display: region_display_name.clone(),
                password: password.to_owned(),
                tier: "Unranked".to_owned(),
                reached_last_season: "N/A".to_owned(),
                finished_last_season: "N/A".to_owned(),
                ..Account::default()
            };
            if self.manager.add_account(account.clone()).is_ok() {
                added_accounts.push(account);
                added += 1;
            } else {
                skipped += 1;
            }
        }
        self.multi_add.clear();
        if !added_accounts.is_empty() {
            self.start_rank_jobs(added_accounts);
        }
        self.status = format!("Multi-add complete: {added} added, {skipped} skipped.");
    }

    fn start_rank_jobs(&mut self, accounts: Vec<Account>) {
        if accounts.is_empty() {
            return;
        }
        let provider = Arc::clone(&self.manager.rank_fetcher);
        let jobs: Vec<(AccountKey, Account)> = accounts
            .into_iter()
            .map(|account| (account.key(), account))
            .collect();
        let pending = jobs.len();
        let (sender, receiver) = if let Some(sender) = self.rank_sender.clone() {
            (sender, None)
        } else {
            let (sender, receiver) = mpsc::channel();
            (sender, Some(receiver))
        };
        let queue = Arc::new(Mutex::new(VecDeque::from(jobs)));
        let worker_count = pending.min(4);
        for _ in 0..worker_count {
            let queue = Arc::clone(&queue);
            let sender = sender.clone();
            let provider = Arc::clone(&provider);
            thread::spawn(move || loop {
                let job = queue.lock().ok().and_then(|mut queue| queue.pop_front());
                let Some((key, account)) = job else { break };
                let info = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    provider.fetch_rank(&account)
                }))
                .unwrap_or_else(|_| RankInfo::error());
                if sender.send(RankUpdate { key, info }).is_err() {
                    break;
                }
            });
        }
        if let Some(receiver) = receiver {
            self.rank_updates = Some(receiver);
            self.rank_sender = Some(sender);
            self.rank_jobs_pending = pending;
        } else {
            self.rank_jobs_pending = self.rank_jobs_pending.saturating_add(pending);
        }
    }

    fn poll_rank_jobs(&mut self) {
        let mut updates = Vec::new();
        if let Some(receiver) = self.rank_updates.as_ref() {
            while let Ok(update) = receiver.try_recv() {
                updates.push(update);
            }
        }
        for update in updates {
            self.manager.apply_rank_info(&update.key, &update.info);
            self.rank_jobs_pending = self.rank_jobs_pending.saturating_sub(1);
        }
        if self.rank_updates.is_some() && self.rank_jobs_pending == 0 {
            self.rank_updates = None;
            self.rank_sender = None;
            sort_accounts(&mut self.manager.accounts);
            if let Err(error) = self.manager.save_accounts() {
                self.status = format!("Rank update save failed: {error}");
            } else {
                self.status = "Rank updates complete.".to_owned();
            }
        }
    }

    fn refresh_all_ranks(&mut self) {
        if self.rank_updates.is_some() {
            self.status = "A rank refresh is already running.".to_owned();
            return;
        }
        let accounts = self.manager.accounts.clone();
        if accounts.is_empty() {
            self.status = "No accounts to refresh.".to_owned();
            return;
        }
        self.start_rank_jobs(accounts);
        self.status = "Refreshing ranks…".to_owned();
    }

    fn copy_account_id(&mut self) {
        let Some(account) = self.selected_account() else {
            return;
        };
        match arboard::Clipboard::new()
            .and_then(|mut clipboard| clipboard.set_text(account.account_id.clone()))
        {
            Ok(()) => self.status = "Account ID copied.".to_owned(),
            Err(error) => self.status = format!("Clipboard error: {error}"),
        }
    }

    fn copy_password(&mut self) {
        let Some(account) = self.selected_account() else {
            return;
        };
        let password = if account.password.is_empty() {
            credentials::get_password(
                KEYRING_SERVICE,
                &format!("{}:{}", account.region, account.account_id),
            )
            .ok()
            .flatten()
            .unwrap_or_default()
        } else {
            account.password
        };
        if password.is_empty() {
            self.status = "No password is stored for this account.".to_owned();
            return;
        }
        match arboard::Clipboard::new().and_then(|mut clipboard| clipboard.set_text(password)) {
            Ok(()) => self.status = "Password copied.".to_owned(),
            Err(error) => self.status = format!("Clipboard error: {error}"),
        }
    }

    fn copy_shortcut(&mut self) {
        let Some(key) = self.selected.clone() else {
            return;
        };
        if self.last_copied.as_ref() != Some(&key) {
            self.last_copied = Some(key);
            self.copy_counter = 0;
        }
        if self.copy_counter.is_multiple_of(2) {
            self.copy_account_id();
        } else {
            self.copy_password();
        }
        self.copy_counter = self.copy_counter.wrapping_add(1);
    }

    fn auto_type_selected(&mut self) {
        let Some(account) = self.selected_account() else {
            return;
        };
        let password = if account.password.is_empty() {
            credentials::get_password(
                KEYRING_SERVICE,
                &format!("{}:{}", account.region, account.account_id),
            )
            .ok()
            .flatten()
            .unwrap_or_default()
        } else {
            account.password
        };
        self.status = "Switching to the previous window and entering credentials…".to_owned();
        let completed = auto_type_credentials(&account.account_id, &password);
        self.status = if completed {
            "Switched to the previous window and entered credentials.".to_owned()
        } else {
            "Auto-type failed: shortcut keys were not released, or clipboard/keyboard input was rejected.".to_owned()
        };
    }

    fn delete_selected(&mut self) {
        let Some(account) = self.selected_account() else {
            return;
        };
        let old_index = self
            .manager
            .accounts
            .iter()
            .position(|candidate| candidate.key() == account.key())
            .unwrap_or(0);
        match self
            .manager
            .delete_account(&account.account_id, &account.region)
        {
            Ok(()) => {
                self.selected = self
                    .manager
                    .accounts
                    .get(old_index.min(self.manager.accounts.len().saturating_sub(1)))
                    .map(Account::key);
                self.status = format!("Deleted {}.", account.account_id);
            }
            Err(error) => self.status = format!("Delete failed: {error}"),
        }
    }

    fn export_data(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .set_file_name("credentials.json")
            .add_filter("JSON files", &["json"])
            .save_file()
        else {
            return;
        };
        match self
            .manager
            .export_accounts()
            .and_then(|json| std::fs::write(&path, json).map_err(Into::into))
        {
            Ok(()) => self.status = format!("Accounts exported to {}.", path.display()),
            Err(error) => self.status = format!("Export failed: {error}"),
        }
    }

    fn import_data(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter("JSON files", &["json"])
            .pick_file()
        else {
            return;
        };
        let result = std::fs::read_to_string(&path)
            .map_err(Into::into)
            .and_then(|json: String| self.manager.import_accounts(&json));
        match result {
            Ok((added, skipped)) => {
                self.status = format!("Import complete: {added} added, {skipped} skipped.")
            }
            Err(error) => self.status = format!("Import failed: {error}"),
        }
    }

    fn show_edit(&mut self, account: &Account, field: EditField) {
        self.edit_state = Some(EditState {
            key: account.key(),
            field,
            value: match field {
                EditField::Name => account.name.clone(),
                EditField::Description => account.description.clone(),
            },
        });
    }

    fn save_edit(&mut self) {
        let Some(edit) = self.edit_state.take() else {
            return;
        };
        if let Some(account) = self.manager.account_mut(&edit.key) {
            match edit.field {
                EditField::Name => account.name = edit.value,
                EditField::Description => account.description = edit.value,
            }
            if let Err(error) = self.manager.save_accounts() {
                self.status = format!("Could not save edit: {error}");
            }
        }
    }

    fn can_play_with(&self, account: &Account, friend_tier: &str, friend_division: &str) -> bool {
        fn division_to_int(division: &str) -> Option<i32> {
            match division.trim().to_ascii_uppercase().as_str() {
                "I" | "1" => Some(1),
                "II" | "2" => Some(2),
                "III" | "3" => Some(3),
                "IV" | "4" => Some(4),
                _ => None,
            }
        }
        let account_tier = account.tier.to_ascii_lowercase();
        let friend_tier = friend_tier.to_ascii_lowercase();
        let account_division = division_to_int(&account.division);
        let friend_division = division_to_int(friend_division);
        match friend_tier.as_str() {
            "master" => {
                account_tier == "master"
                    || (account_tier == "diamond" && account_division == Some(1))
            }
            "diamond" => {
                if account_tier == "diamond" {
                    if let (Some(account_division), Some(friend_division)) =
                        (account_division, friend_division)
                    {
                        if (account_division - friend_division).abs() <= 2 {
                            return true;
                        }
                    }
                }
                (account_tier == "emerald"
                    && account_division == Some(1)
                    && friend_division == Some(4))
                    || (account_tier == "master" && friend_division == Some(1))
            }
            "emerald" => {
                account_tier == "emerald"
                    || account_tier == "platinum"
                    || (friend_division == Some(1)
                        && account_tier == "diamond"
                        && account_division == Some(4))
            }
            "platinum" => matches!(account_tier.as_str(), "platinum" | "emerald" | "gold"),
            "iron" => matches!(account_tier.as_str(), "iron" | "bronze" | "silver"),
            "bronze" | "silver" | "gold" => {
                let Some(index) = TIER_ORDER
                    .iter()
                    .position(|tier| tier.eq_ignore_ascii_case(friend_tier.as_str()))
                else {
                    return false;
                };
                let mut allowed = Vec::new();
                if index > 0 {
                    allowed.push(TIER_ORDER[index - 1]);
                }
                allowed.push(TIER_ORDER[index]);
                if index < TIER_ORDER.len().saturating_sub(3) {
                    allowed.push(TIER_ORDER[index + 1]);
                }
                allowed
                    .iter()
                    .any(|tier| tier.eq_ignore_ascii_case(account_tier.as_str()))
            }
            _ => false,
        }
    }

    fn show_ranks_window(&mut self, ctx: &Context) {
        if !self.show_ranks {
            return;
        }
        if self.ranks_texture.is_none() {
            if let Ok(image) = image::load_from_memory(RANKS_IMAGE) {
                let image = image.to_rgba8();
                let size = [image.width() as usize, image.height() as usize];
                self.ranks_texture = Some(ctx.load_texture(
                    "rank-compatibilities",
                    egui::ColorImage::from_rgba_unmultiplied(size, image.as_raw()),
                    TextureOptions::LINEAR,
                ));
            }
        }
        let mut open = true;
        egui::Window::new("Ranks Compatibilities")
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .show(ctx, |ui| {
                if let Some(texture) = &self.ranks_texture {
                    let available = ui.available_size();
                    let scale = (available.x / texture.size_vec2().x)
                        .min(available.y / texture.size_vec2().y)
                        .min(1.0);
                    ui.image((texture.id(), texture.size_vec2() * scale));
                } else {
                    ui.label("Ranks image could not be loaded.");
                }
            });
        if !open {
            self.show_ranks = false;
        }
    }
}

impl LeagueAccountsApp {
    fn render(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.poll_rank_jobs();
        let mut table_cell_ids = Vec::new();

        egui::Panel::top("header").show_inside(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading(
                    RichText::new("League Accounts").color(Color32::from_rgb(220, 228, 238)),
                );
                ui.separator();
                ui.label("Search:");
                ui.add(
                    egui::TextEdit::singleline(&mut self.search)
                        .id(egui::Id::new("search"))
                        .hint_text("Filter by name or account ID…")
                        .desired_width(300.0),
                );
                if !self.status.is_empty() {
                    ui.separator();
                    ui.label(RichText::new(&self.status).weak());
                }
            });
        });

        egui::CentralPanel::default().show_inside(ui, |ui| {
            // The bounded table must leave space for the forms. The outer
            // scroll area keeps every control reachable on smaller windows.
            egui::ScrollArea::vertical()
                .id_salt("main_content_scroll")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.allocate_ui_with_layout(
                        egui::vec2(ui.available_width(), TABLE_HEIGHT),
                        egui::Layout::left_to_right(egui::Align::Min),
                        |ui| {
                        ui.allocate_ui_with_layout(
                            egui::vec2(
                                (ui.available_width() - 170.0).max(400.0),
                                TABLE_HEIGHT,
                            ),
                            egui::Layout::top_down(egui::Align::Min),
                            |ui| {
                                egui::Frame::group(ui.style()).show(ui, |ui| {
                                    egui::ScrollArea::both()
                                        .id_salt("accounts_scroll")
                                        .max_height(TABLE_HEIGHT)
                                        .auto_shrink([false, false])
                                        .show(ui, |ui| {
                                            egui::Grid::new("accounts_table")
                                                .striped(true)
                                                .min_col_width(80.0)
                                                .spacing([10.0, 5.0])
                                                .show(ui, |ui| {
                                                    for header in [
                                                        "Account ID",
                                                        "Summoner Name",
                                                        "Region",
                                                        "Level",
                                                        "Tier",
                                                        "Division",
                                                        "LP",
                                                        "Reached Last Season",
                                                        "Finished Last Season",
                                                        "Description",
                                                    ] {
                                                        ui.label(RichText::new(header).strong());
                                                    }
                                                    ui.end_row();
                                                    for account in self.visible_accounts() {
                                                        let selected = self.selected.as_ref()
                                                            == Some(&account.key());
                                                        let values = [
                                                            account.account_id.clone(),
                                                            account.name.clone(),
                                                            account.region_display.clone(),
                                                            if account.level.is_empty() {
                                                                "...".to_owned()
                                                            } else {
                                                                account.level.clone()
                                                            },
                                                            if account.tier.is_empty()
                                                                || account.tier == "Unranked"
                                                            {
                                                                "...".to_owned()
                                                            } else {
                                                                account.tier.clone()
                                                            },
                                                            if account.division.is_empty() {
                                                                "...".to_owned()
                                                            } else {
                                                                account.division.clone()
                                                            },
                                                            if account.lp.is_empty() {
                                                                "...".to_owned()
                                                            } else {
                                                                account.lp.clone()
                                                            },
                                                            if account.reached_last_season.is_empty() {
                                                                "N/A".to_owned()
                                                            } else {
                                                                account.reached_last_season.clone()
                                                            },
                                                            if account.finished_last_season.is_empty() {
                                                                "N/A".to_owned()
                                                            } else {
                                                                account.finished_last_season.clone()
                                                            },
                                                            account.description.clone(),
                                                        ];
                                                        for (column, value) in values.iter().enumerate() {
                                                            // Keep focus attached to the account,
                                                            // not its position after filtering/sorting.
                                                            let id = ui.make_persistent_id((account.key(), column));
                                                            let response = ui.push_id(id, |ui| {
                                                                ui.selectable_label(selected, value)
                                                            }).inner;
                                                            table_cell_ids.push(response.id);
                                                            if response.clicked() {
                                                                self.selected = Some(account.key());
                                                                response.request_focus();
                                                            }
                                                            if response.double_clicked()
                                                                && (column == 1 || column == 9)
                                                            {
                                                                self.show_edit(
                                                                    &account,
                                                                    if column == 1 {
                                                                        EditField::Name
                                                                    } else {
                                                                        EditField::Description
                                                                    },
                                                                );
                                                            }
                                                        }
                                                        ui.end_row();
                                                    }
                                                });
                                        });
                                });
                            },
                        );
                        ui.separator();
                        ui.vertical(|ui| {
                            ui.heading("Actions");
                            ui.add_enabled_ui(self.selected.is_some(), |ui| {
                                if ui.button("Copy Account ID").clicked() {
                                    self.copy_account_id();
                                }
                                if ui.button("Copy Password").clicked() {
                                    self.copy_password();
                                }
                            });
                            if ui.button("Refresh Ranks").clicked() {
                                self.refresh_all_ranks();
                            }
                            if ui.button("Export Data").clicked() {
                                self.export_data();
                            }
                            if ui.button("Import Data").clicked() {
                                self.import_data();
                            }
                            if ui.button("Shortcuts Help").clicked() {
                                self.show_help = true;
                            }
                        });
                    });

                    ui.add_space(10.0);
                    egui::Frame::group(ui.style()).show(ui, |ui| {
                        ui.heading("Add New Account");
                        egui::Grid::new("add_form")
                            .num_columns(2)
                            .spacing([8.0, 6.0])
                            .show(ui, |ui| {
                                ui.label("Account ID:");
                                ui.text_edit_singleline(&mut self.add_account_id);
                                ui.end_row();
                                ui.label("Summoner Name:");
                                ui.text_edit_singleline(&mut self.add_name);
                                ui.end_row();
                                ui.label("Region:");
                                egui::ComboBox::from_id_salt("add_region")
                                    .selected_text(&self.add_region)
                                    .show_ui(ui, |ui| {
                                        for (label, _) in REGION_MAP {
                                            ui.selectable_value(
                                                &mut self.add_region,
                                                (*label).to_owned(),
                                                *label,
                                            );
                                        }
                                    });
                                ui.end_row();
                                ui.label("Password:");
                                ui.add(egui::TextEdit::singleline(&mut self.add_password).password(true));
                                ui.end_row();
                                ui.label("Description:");
                                ui.text_edit_singleline(&mut self.add_description);
                                ui.end_row();
                            });
                        let add_button = ui.button("Add Account");
                        let add_with_enter = add_button.has_focus()
                            && ctx.input(|input| input.key_pressed(Key::Enter));
                        if add_button.clicked() || add_with_enter {
                            self.add_account();
                        }
                        ui.separator();
                        ui.heading("Multi Add");
                        ui.label("One account per line: AccountID--InGameName#TAG--password");
                        ui.add(
                            egui::TextEdit::multiline(&mut self.multi_add)
                                .desired_rows(3)
                                .desired_width(f32::INFINITY),
                        );
                        if ui.button("Multi Add").clicked() {
                            self.multi_add_accounts();
                        }
                    });

                    ui.add_space(10.0);
                    egui::Frame::group(ui.style()).show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label("Friend Elo:");
                            egui::ComboBox::from_id_salt("friend_tier")
                                .selected_text(&self.friend_tier)
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(
                                        &mut self.friend_tier,
                                        "Show All".to_owned(),
                                        "Show All",
                                    );
                                    for tier in TIER_ORDER.iter().take(TIER_ORDER.len() - 2) {
                                        if !matches!(*tier, "Challenger" | "Grandmaster") {
                                            ui.selectable_value(
                                                &mut self.friend_tier,
                                                (*tier).to_owned(),
                                                *tier,
                                            );
                                        }
                                    }
                                });
                            egui::ComboBox::from_id_salt("friend_division")
                                .selected_text(&self.friend_division)
                                .show_ui(ui, |ui| {
                                    for division in ["I", "II", "III", "IV"] {
                                        ui.selectable_value(
                                            &mut self.friend_division,
                                            division.to_owned(),
                                            division,
                                        );
                                    }
                                });
                            if ui.button("Ranks Compatibilities").clicked() {
                                self.show_ranks = true;
                            }
                        });
                    });
                });
        });

        if let Some(edit) = &mut self.edit_state {
            let mut save = false;
            let mut cancel = false;
            egui::Window::new(match edit.field {
                EditField::Name => "Edit Summoner Name",
                EditField::Description => "Edit Description",
            })
            .collapsible(false)
            .resizable(false)
            .show(&ctx, |ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut edit.value)
                        .id(egui::Id::new("account_edit")),
                );
                ui.horizontal(|ui| {
                    save = ui.button("Save").clicked();
                    cancel = ui.button("Cancel").clicked();
                });
            });
            if save {
                self.save_edit();
            }
            if cancel {
                self.edit_state = None;
            }
        }
        if self.show_help {
            egui::Window::new("Shortcuts Help").open(&mut self.show_help).resizable(false).show(&ctx, |ui| {
                ui.label("Keyboard Shortcuts:");
                ui.label("• Ctrl+C in the account table: Copy Account ID (press again to copy Password)");
                ui.label("• Ctrl+Shift+V: Auto-type Account ID and Password into another window");
                ui.label("• Delete in the account table: Delete selected account");
                ui.label("• Double-click Summoner Name or Description: Edit in place");
                ui.label("• Enter on Add Account: Add a new account");
                ui.separator();
                ui.label("Use Search to filter by name or ID, and Friend Elo to show compatible accounts.");
            });
        }
        self.show_ranks_window(&ctx);

        // Check focus after all widgets have handled input. A selected account
        // alone is not permission to steal Delete or Copy from a text editor.
        let table_has_focus = self.selected.is_some()
            && self.edit_state.is_none()
            && !self.show_help
            && !self.show_ranks
            && ctx.memory(|memory| {
                memory.focused().is_some_and(|id| table_cell_ids.contains(&id))
            });
        let actions = shortcuts::take_table_actions(&ctx, table_has_focus);
        if actions.delete {
            self.delete_selected();
        } else if actions.copy {
            self.copy_shortcut();
        }

        // Native Windows events are intercepted before clipboard translation.
        // Keep the key-event path for integrations that pass the chord through.
        let auto_type = ctx.input_mut(|input| {
            input.consume_key(Modifiers::CTRL | Modifiers::SHIFT, Key::V)
        });
        #[cfg(windows)]
        let auto_type = self.native_auto_type.as_ref()
            .is_some_and(shortcuts::NativeAutoType::take_requested) || auto_type;
        if auto_type {
            self.auto_type_selected();
        }
        ctx.request_repaint_after(Duration::from_millis(100));
    }
}

impl eframe::App for LeagueAccountsApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.render(ui);
    }
}

fn auto_type_credentials(account_id: &str, password: &str) -> bool {
    // Do not inject input if the physical shortcut is still held at timeout.
    if !wait_for_shortcut_release() {
        return false;
    }
    let Ok(mut clipboard) = arboard::Clipboard::new() else {
        return false;
    };
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
            KEYEVENTF_KEYUP, VK_MENU, VK_RETURN, VK_TAB,
        };

        // Put the first value on the clipboard before leaving this window.
        // SendInput is used instead of keybd_event because it preserves the
        // Alt+Tab transition and is handled consistently by modern Windows
        // applications (including the Riot Client).
        if clipboard.set_text(account_id.to_owned()).is_err() || !send_key(VK_MENU, 0) {
            return false;
        }
        // Give the shell a moment to register Alt before pressing Tab. A
        // single back-to-back SendInput batch is intermittently ignored by
        // the Windows task switcher on slower desktops.
        thread::sleep(Duration::from_millis(35));
        if !send_key(VK_TAB, 0) {
            let _ = send_key(VK_MENU, KEYEVENTF_KEYUP);
            return false;
        }
        thread::sleep(Duration::from_millis(35));
        let tab_released = send_key(VK_TAB, KEYEVENTF_KEYUP);
        let alt_released = send_key(VK_MENU, KEYEVENTF_KEYUP);
        if !tab_released || !alt_released {
            return false;
        }
        thread::sleep(Duration::from_millis(400));
        if !paste_current_clipboard() {
            return false;
        }
        thread::sleep(Duration::from_millis(60));
        let tab_down = send_key(VK_TAB, 0);
        let tab_up = send_key(VK_TAB, KEYEVENTF_KEYUP);
        if !tab_down || !tab_up {
            return false;
        }
        if !password.is_empty() {
            thread::sleep(Duration::from_millis(60));
            if clipboard.set_text(password.to_owned()).is_err() {
                return false;
            }
            // arboard updates the process clipboard asynchronously on some
            // Windows versions; let the new value become visible before Ctrl+V.
            thread::sleep(Duration::from_millis(60));
            if !paste_current_clipboard() {
                return false;
            }
        }
        thread::sleep(Duration::from_millis(60));
        let enter_down = send_key(VK_RETURN, 0);
        let enter_up = send_key(VK_RETURN, KEYEVENTF_KEYUP);
        enter_down && enter_up
    }
    #[cfg(not(windows))]
    {
        clipboard
            .set_text(format!("{account_id}\n{password}"))
            .is_ok()
    }
}

#[cfg(windows)]
fn wait_for_shortcut_release() -> bool {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, VK_CONTROL, VK_SHIFT, VK_V,
    };

    for _ in 0..100 {
        let shortcut_still_down = [VK_CONTROL, VK_SHIFT, VK_V].into_iter().any(|key| {
            // GetAsyncKeyState's high bit is set while the physical key is
            // down. The low bit is intentionally ignored because it reports
            // historical presses rather than current modifier state.
            unsafe { GetAsyncKeyState(key as i32) < 0 }
        });
        if !shortcut_still_down {
            thread::sleep(Duration::from_millis(20));
            return true;
        }
        thread::sleep(Duration::from_millis(10));
    }
    false
}

#[cfg(not(windows))]
fn wait_for_shortcut_release() -> bool {
    thread::sleep(Duration::from_millis(120));
    true
}

#[cfg(windows)]
fn send_key(virtual_key: u16, flags: u32) -> bool {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT,
    };

    let input = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: virtual_key,
                wScan: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    unsafe { SendInput(1, &input, std::mem::size_of::<INPUT>() as i32) == 1 }
}

#[cfg(windows)]
fn paste_current_clipboard() -> bool {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{KEYEVENTF_KEYUP, VK_CONTROL};
    // Always send both key-up events, even if a key-down event is rejected,
    // so a transient SendInput failure cannot leave Ctrl or V stuck down.
    let control_down = send_key(VK_CONTROL, 0);
    let v_down = send_key(b'V' as u16, 0);
    let v_up = send_key(b'V' as u16, KEYEVENTF_KEYUP);
    let control_up = send_key(VK_CONTROL, KEYEVENTF_KEYUP);
    control_down && v_down && v_up && control_up
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("League Accounts")
            .with_maximized(true),
        ..Default::default()
    };
    eframe::run_native(
        "League Accounts",
        options,
        Box::new(|cc| Ok(Box::new(LeagueAccountsApp::new(cc)))),
    )
}

#[cfg(test)]
#[path = "app_tests.rs"]
mod app_tests;
