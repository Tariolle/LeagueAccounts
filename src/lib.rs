//! Core functionality for LeagueAccounts.
//!
//! The desktop front-end lives in `main.rs`; this module intentionally keeps
//! account storage, credential handling, and OP.GG parsing independent so the
//! behavior can be exercised without starting a window.

pub mod account_manager;
pub mod credentials;
pub mod models;
pub mod rank_fetcher;
pub mod utils;

pub use account_manager::AccountManager;
pub use models::{Account, RankInfo};
pub use rank_fetcher::{RankFetcher, RankProvider};
