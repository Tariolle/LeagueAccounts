//! Exercises the actual reqwest provider. Public IDs only; no saved accounts read.
//! cargo run --example opgg_probe -- --force "kr:Hide on bush#KR1" "kr:Gumayusi#KR1" "kr:Oner#KR1"
use leagueaccounts::models::Account;
use leagueaccounts::rank_fetcher::{RankFetcher, RefreshMode};
use std::sync::Arc;
use std::time::Instant;

fn main() {
    let mut mode = RefreshMode::Manual;
    let mut accounts = Vec::new();
    for argument in std::env::args().skip(1) {
        if argument == "--force" {
            mode = RefreshMode::Force;
        } else {
            let (region, name) = argument
                .split_once(':')
                .expect("Expected region:Game Name#TAG");
            assert!(
                accounts.len() < 3,
                "At most three public profiles per probe"
            );
            accounts.push(Account {
                region: region.into(),
                name: name.into(),
                ..Default::default()
            });
        }
    }
    assert!(!accounts.is_empty(), "Supply one to three public Riot IDs");
    let fetcher = Arc::new(RankFetcher::new());
    let started = Instant::now();
    std::thread::scope(|scope| {
        let workers: Vec<_> = accounts
            .into_iter()
            .map(|account| {
                let fetcher = Arc::clone(&fetcher);
                scope.spawn(move || {
                let started = Instant::now();
                let info = fetcher.fetch_rank_with_mode(&account, mode);
                println!("{}", serde_json::json!({
                    "profile":format!("{}:{}",account.region,account.name),
                    "seconds":started.elapsed().as_secs_f64(),
                    "tier":info.tier,"division":info.division,"lp":info.lp,"level":info.level,
                    "recentGames":info.recent_games,
                    "notFound":info.riot_id_not_found,"provenance":info.provenance,
                    "tft":info.tft,"tftProvenance":info.tft_provenance
                }));
            })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
    });
    println!("total_seconds={:.3}", started.elapsed().as_secs_f64());
}
