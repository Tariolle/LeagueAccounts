//! Windows Credential Manager access.
//!
//! The `keyring` crate selects the native Windows backend on Windows. The
//! small fallback lookup mirrors the legacy Python implementation, which can
//! still find credentials created under the service-only target name.

use std::error::Error;

pub const SERVICE: &str = "LeagueAccounts";

type CredentialResult<T> = Result<T, Box<dyn Error + Send + Sync>>;

pub fn get_password(service: &str, username: &str) -> CredentialResult<Option<String>> {
    let primary_target = format!("{username}@{service}");
    let primary = keyring::Entry::new_with_target(&primary_target, service, username)?;
    match primary.get_password() {
        Ok(password) => return Ok(Some(password)),
        Err(keyring::Error::NoEntry) => {}
        Err(error) => return Err(Box::new(error)),
    }

    // Older versions of the app/keyring could use the service as the target
    // and put the username in the credential metadata. keyring exposes the
    // target independently, so try it as a compatibility lookup.
    let legacy = keyring::Entry::new_with_target(service, service, username)?;
    let attributes = match legacy.get_attributes() {
        Ok(attributes) => attributes,
        Err(keyring::Error::NoEntry) => return Ok(None),
        Err(error) => return Err(Box::new(error)),
    };
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
    // Migrate a legacy service-only entry before replacing it with the
    // account-specific target. This preserves credentials created by the
    // previous implementation when a second account is added.
    let legacy = keyring::Entry::new_with_target(service, service, username)?;
    if let Ok(attributes) = legacy.get_attributes() {
        if let Some(existing_user) = attributes.get("username") {
            if existing_user != username {
                if let Ok(existing_password) = legacy.get_password() {
                    let migrated_target = format!("{existing_user}@{service}");
                    if let Ok(migrated) =
                        keyring::Entry::new_with_target(&migrated_target, service, existing_user)
                    {
                        let _ = migrated.set_password(&existing_password);
                    }
                }
            }
        }
    }
    let primary_target = format!("{username}@{service}");
    let entry = keyring::Entry::new_with_target(&primary_target, service, username)?;
    entry.set_password(password)?;
    Ok(())
}

pub fn delete_password(service: &str, username: &str) -> CredentialResult<()> {
    let primary_target = format!("{username}@{service}");
    let entry = keyring::Entry::new_with_target(&primary_target, service, username)?;
    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(error) => Err(Box::new(error)),
    }
}
