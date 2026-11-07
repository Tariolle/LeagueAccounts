//! Windows Credential Manager access.
//!
//! The `keyring` crate selects the native Windows backend on Windows. Normal
//! reads, writes and rollback use only the requested account's explicit target.
//! Account loading copies a matching legacy-only entry to that target and
//! removes the shared legacy entry only after the copy is safe; deleting an
//! account clears both copies. No function logs a username, target or secret.

use crate::logging::{self, Event, Reason};
use std::error::Error;

pub const SERVICE: &str = "LeagueAccounts";

type CredentialResult<T> = Result<T, Box<dyn Error + Send + Sync>>;

pub fn get_password(service: &str, username: &str) -> CredentialResult<Option<String>> {
    get_password_inner(service, username).inspect_err(|_| {
        logging::record(Event::CredentialReadFailed, Reason::CredentialStore);
    })
}

fn primary_entry(service: &str, username: &str) -> CredentialResult<keyring::Entry> {
    let primary_target = format!("{username}@{service}");
    Ok(keyring::Entry::new_with_target(
        &primary_target,
        service,
        username,
    )?)
}

fn get_password_inner(service: &str, username: &str) -> CredentialResult<Option<String>> {
    match primary_entry(service, username)?.get_password() {
        Ok(password) => Ok(Some(password)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(Box::new(error)),
    }
}

/// The legacy entry shared by every account on a service.
fn legacy_entry(service: &str, username: &str) -> CredentialResult<keyring::Entry> {
    Ok(keyring::Entry::new_with_target(service, service, username)?)
}

fn matching_legacy_attributes(
    service: &str,
    username: &str,
) -> CredentialResult<Option<std::collections::HashMap<String, String>>> {
    let legacy = legacy_entry(service, username)?;
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
    Ok(Some(attributes))
}

/// Load an existing account's password, when needed upgrading a matching
/// legacy-only entry.
///
/// Call this only while loading saved accounts, never during a transaction or
/// rollback. A primary credential (even an empty one) is always authoritative;
/// a matching legacy copy is then stale and removed without touching another
/// account's credential. A legacy-only value is copied first, and the legacy
/// entry is deleted only after that write succeeds.
pub fn load_password_with_migration(
    service: &str,
    username: &str,
) -> CredentialResult<Option<String>> {
    if let Some(password) = get_password(service, username)? {
        if delete_matching_legacy(service, username).is_err() {
            logging::record(Event::CredentialDeleteFailed, Reason::CredentialStore);
        }
        return Ok(Some(password));
    }
    let password = get_legacy_password(service, username).inspect_err(|_| {
        logging::record(Event::CredentialReadFailed, Reason::CredentialStore);
    })?;
    if let Some(password) = &password {
        // set_password logs failures. A failed copy leaves the readable legacy
        // entry for a later load and still lets this session sign in.
        if set_password(service, username, password).is_ok()
            && delete_matching_legacy(service, username).is_err()
        {
            logging::record(Event::CredentialDeleteFailed, Reason::CredentialStore);
        }
    }
    Ok(password)
}

fn get_legacy_password(service: &str, username: &str) -> CredentialResult<Option<String>> {
    let legacy = legacy_entry(service, username)?;
    if matching_legacy_attributes(service, username)?.is_none() {
        return Ok(None);
    }
    match legacy.get_password() {
        Ok(password) => Ok(Some(password)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(Box::new(error)),
    }
}

/// Remove the shared legacy entry only while it belongs to `username`.
fn delete_matching_legacy(service: &str, username: &str) -> CredentialResult<()> {
    if matching_legacy_attributes(service, username)?.is_none() {
        return Ok(());
    }
    let legacy = legacy_entry(service, username)?;
    match legacy.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(error) => Err(Box::new(error)),
    }
}

pub fn set_password(service: &str, username: &str, password: &str) -> CredentialResult<()> {
    set_password_inner(service, username, password).inspect_err(|_| {
        logging::record(Event::CredentialWriteFailed, Reason::CredentialStore);
    })
}

fn set_password_inner(service: &str, username: &str, password: &str) -> CredentialResult<()> {
    primary_entry(service, username)?.set_password(password)?;
    Ok(())
}

/// Delete the explicit target and a matching legacy copy as one transaction.
pub fn delete_password(service: &str, username: &str) -> CredentialResult<()> {
    delete_password_inner(service, username).inspect_err(|_| {
        logging::record(Event::CredentialDeleteFailed, Reason::CredentialStore);
    })
}

fn delete_password_inner(service: &str, username: &str) -> CredentialResult<()> {
    let previous = get_password_inner(service, username)?;
    match primary_entry(service, username)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => {}
        Err(error) => return Err(Box::new(error)),
    }
    if let Err(error) = delete_matching_legacy(service, username) {
        // Restore the primary copy so a failed cleanup does not lose the
        // credential; the matching legacy entry is still present as well.
        if let Some(password) = previous {
            set_password_inner(service, username, &password)?;
        }
        return Err(error);
    }
    Ok(())
}

/// Complete target state kept only inside account-file transactions. Rollback
/// must restore a matching legacy copy as well as the primary credential.
pub(crate) struct PasswordSnapshot {
    pub(crate) primary: Option<String>,
    pub(crate) legacy: Option<String>,
}

pub(crate) fn snapshot_password(
    service: &str,
    username: &str,
) -> CredentialResult<PasswordSnapshot> {
    let result: CredentialResult<PasswordSnapshot> = (|| {
        Ok(PasswordSnapshot {
            primary: get_password_inner(service, username)?,
            legacy: get_legacy_password(service, username)?,
        })
    })();
    result.inspect_err(|_| {
        logging::record(Event::CredentialReadFailed, Reason::CredentialStore);
    })
}

pub(crate) fn restore_password(
    service: &str,
    username: &str,
    snapshot: PasswordSnapshot,
) -> CredentialResult<()> {
    let result: CredentialResult<()> = (|| {
        match snapshot.primary {
            Some(password) => set_password_inner(service, username, &password)?,
            None => match primary_entry(service, username)?.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => {}
                Err(error) => return Err(Box::new(error) as Box<dyn Error + Send + Sync>),
            },
        }
        if let Some(password) = snapshot.legacy {
            let legacy = legacy_entry(service, username)?;
            match legacy.get_attributes() {
                Ok(attributes)
                    if attributes.get("username").map(String::as_str) != Some(username) =>
                {
                    // The shared entry changed owners during the transaction;
                    // never replace another account's credential on rollback.
                    return Err(std::io::Error::other("legacy credential changed owner").into());
                }
                Ok(_) | Err(keyring::Error::NoEntry) => {}
                Err(error) => return Err(Box::new(error) as Box<dyn Error + Send + Sync>),
            }
            legacy.set_password(&password)?;
        }
        Ok(())
    })();
    result.inspect_err(|_| {
        logging::record(Event::CredentialWriteFailed, Reason::CredentialStore);
    })
}
