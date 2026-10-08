//! Windows Credential Manager access.
//!
//! The `keyring` crate selects the native Windows backend on Windows. Normal
//! reads, writes and rollback use only the requested account's explicit target.
//! Account loading has a separate, non-destructive legacy migration path.

use crate::logging::{self, Event, Reason};
use std::error::Error;

pub const SERVICE: &str = "LeagueAccounts";

type CredentialResult<T> = Result<T, Box<dyn Error + Send + Sync>>;

pub fn get_password(service: &str, username: &str) -> CredentialResult<Option<String>> {
    get_password_inner(service, username).inspect_err(|_| {
        logging::record(Event::CredentialReadFailed, Reason::CredentialStore);
    })
}

fn get_password_inner(service: &str, username: &str) -> CredentialResult<Option<String>> {
    let primary_target = format!("{username}@{service}");
    let primary = keyring::Entry::new_with_target(&primary_target, service, username)?;
    match primary.get_password() {
        Ok(password) => Ok(Some(password)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(Box::new(error)),
    }
}

/// Load an existing account's password, upgrading a matching legacy-only entry.
///
/// Call this only while loading saved accounts, never during a transaction or
/// rollback. A primary credential (even an empty one) is always authoritative.
/// Leave the legacy entry intact so a failed migration cannot lose the secret.
pub fn load_password_with_migration(
    service: &str,
    username: &str,
) -> CredentialResult<Option<String>> {
    if let Some(password) = get_password(service, username)? {
        return Ok(Some(password));
    }
    let password = get_legacy_password(service, username).inspect_err(|_| {
        logging::record(Event::CredentialReadFailed, Reason::CredentialStore);
    })?;
    if let Some(password) = &password {
        // set_password logs failures. Still use the readable legacy password
        // for this session; a later load can retry the non-destructive copy.
        let _ = set_password(service, username, password);
    }
    Ok(password)
}

fn get_legacy_password(service: &str, username: &str) -> CredentialResult<Option<String>> {
    let legacy = keyring::Entry::new_with_target(service, service, username)?;
    let attributes = match legacy.get_attributes() {
        Ok(attributes) => attributes,
        Err(keyring::Error::NoEntry) => return Ok(None),
        Err(error) => return Err(Box::new(error)),
    };
    // The service-only target is shared: its stored username, not the username
    // supplied when constructing Entry, determines which account owns it.
    if attributes.get("username").map(String::as_str) != Some(username) {
        return Ok(None);
    }
    match legacy.get_password() {
        Ok(password) => Ok(Some(password)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(Box::new(error)),
    }
}

pub fn set_password(service: &str, username: &str, password: &str) -> CredentialResult<()> {
    set_password_inner(service, username, password).inspect_err(|_| {
        logging::record(Event::CredentialWriteFailed, Reason::CredentialStore);
    })
}

fn set_password_inner(service: &str, username: &str, password: &str) -> CredentialResult<()> {
    let primary_target = format!("{username}@{service}");
    let entry = keyring::Entry::new_with_target(&primary_target, service, username)?;
    entry.set_password(password)?;
    Ok(())
}

pub fn delete_password(service: &str, username: &str) -> CredentialResult<()> {
    delete_password_inner(service, username).inspect_err(|_| {
        logging::record(Event::CredentialDeleteFailed, Reason::CredentialStore);
    })
}

fn delete_password_inner(service: &str, username: &str) -> CredentialResult<()> {
    let primary_target = format!("{username}@{service}");
    let entry = keyring::Entry::new_with_target(&primary_target, service, username)?;
    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(error) => Err(Box::new(error)),
    }
}
