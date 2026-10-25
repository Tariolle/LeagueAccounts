//! `hosts` patching for Vietnamese accounts.
//!
//! VN sign-in requires Riot's "update your profile" flow, which calls the
//! region's identity provider. With that provider unreachable, Riot retries
//! internally, exhausts its limit and continues anyway — the login takes
//! longer but completes. Blocking the two provider domains in `hosts` makes
//! the retries fail instantly instead of hanging on a connection, and marks
//! the flow as unavailable so the client skips it.
//!
//! The patch is idempotent: existing lines are left alone, missing ones are
//! appended with a marker comment, and `restore` removes only the marked
//! lines. Admin rights are required to write `hosts`; the caller surfaces the
//! error instead of silently skipping, because without the patch the skip flag
//! does nothing.

use crate::logging::{self, Event, Reason};
use std::io::Write;
use std::path::PathBuf;

/// The two VN identity-provider endpoints Riot blocks the login on.
pub const VN_HOSTS: &[&str] = &["login.pp.m.zing.vn", "vn-api.vnggames.app"];

/// Marker so `restore` removes only the lines this tool added.
const MARKER: &str = "# LeagueAccounts VN skip";

fn hosts_path() -> PathBuf {
    PathBuf::from(r"C:\Windows\System32\drivers\etc\hosts")
}

fn entries() -> Vec<String> {
    VN_HOSTS
        .iter()
        .map(|host| format!("127.0.0.1 {host} {MARKER}"))
        .collect()
}

/// Are all VN domains already blocked?
pub fn is_patched() -> bool {
    let Ok(contents) = std::fs::read_to_string(hosts_path()) else {
        return false;
    };
    VN_HOSTS.iter().all(|host| {
        contents.lines().any(|line| {
            line.trim_start().starts_with("127.0.0.1")
                && line.split_whitespace().any(|token| token == *host)
        })
    })
}

/// Append the block entries. Idempotent; admin rights required.
pub fn patch() -> std::io::Result<()> {
    if is_patched() {
        return Ok(());
    }
    let path = hosts_path();
    let mut file = std::fs::OpenOptions::new().append(true).open(&path)?;
    writeln!(file)?; // hosts files conventionally end without a newline.
    for entry in entries() {
        writeln!(file, "{entry}")?;
    }
    file.flush()?;
    logging::record(Event::HostsPatched, Reason::None);
    Ok(())
}

/// Remove the marked lines. Leaves every other line untouched.
pub fn restore() -> std::io::Result<()> {
    let path = hosts_path();
    let contents = std::fs::read_to_string(&path)?;
    let kept: Vec<&str> = contents
        .lines()
        .filter(|line| !line.contains(MARKER))
        .collect();
    let mut file = std::fs::OpenOptions::new().write(true).truncate(true).open(&path)?;
    for line in kept {
        writeln!(file, "{line}")?;
    }
    file.flush()?;
    logging::record(Event::HostsRestored, Reason::None);
    Ok(())
}

/// Patch on creation, restore on drop. `enabled == false` makes it a no-op, so
/// the caller can pass the account flag directly.
pub struct Guard {
    enabled: bool,
}

impl Guard {
    pub fn new(enabled: bool) -> Self {
        if enabled {
            if let Err(error) = patch() {
                // Without admin rights the patch fails. The login continues
                // unpatched: a VN account then waits out Riot's retries and
                // may fail, which the error message already covers.
                logging::record(
                    Event::LoginWaiting,
                    Reason::HostsPatchFailed,
                );
                let _ = error;
            }
        }
        Self { enabled }
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        if self.enabled {
            let _ = restore();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_cover_both_domains_with_the_marker() {
        let entries = entries();
        assert_eq!(entries.len(), VN_HOSTS.len());
        for entry in &entries {
            assert!(entry.starts_with("127.0.0.1 "), "{entry}");
            assert!(entry.contains(MARKER), "{entry}");
        }
        assert!(entries.iter().any(|e| e.contains("login.pp.m.zing.vn")));
        assert!(entries.iter().any(|e| e.contains("vn-api.vnggames.app")));
    }
}

