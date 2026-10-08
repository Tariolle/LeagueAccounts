//! Core functionality for LeagueAccounts.
//!
//! The desktop front-end lives in `src-tauri` (Rust shell) and `ui` (web
//! interface); this crate intentionally keeps
//! account storage, credential handling, and OP.GG parsing independent so the
//! behavior can be exercised without starting a window.

pub mod account_manager;
pub mod autotype;
pub mod clipboard;
pub mod credentials;
pub mod logging;
pub mod models;
pub mod rank_fetcher;
pub mod riot_client;
pub mod updates;
pub mod utils;

pub use account_manager::AccountManager;
pub use models::{Account, RankInfo, TftRank};
pub use rank_fetcher::{RankFetcher, RankProvider};
