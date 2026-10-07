//! Windows Credential Manager access.
//!
//! The `keyring` crate selects the native Windows backend on Windows. Each
//! operation uses only the requested account's explicit target; reads and
//! rollback writes never inspect or modify shared legacy credentials.

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
