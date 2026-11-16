// SPDX-License-Identifier: AGPL-3.0-or-later

//! The OS credential store (service `gitcko`, account `<provider>`).
//!
//! Keyring's error type can carry the stored bytes (`BadEncoding`, `BadDataFormat`).
//! Those values are dropped here. Callers see [`gitcko_config::CredError::Keychain`]
//! and never the secret.

use gitcko_config::{CredError, Keychain};

/// macOS Keychain, Windows Credential Manager, or the Linux Secret Service,
/// via keyring's `v1` feature
/// (<https://docs.rs/keyring/4.2.0/keyring/v1/struct.Entry.html>).
pub struct OsKeychain;

impl Keychain for OsKeychain {
    fn get(&self, account: &str) -> Result<Option<String>, CredError> {
        let entry = keyring::Entry::new("gitcko", account).map_err(|_| CredError::Keychain)?;
        match entry.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(_) => Err(CredError::Keychain),
        }
    }

    fn set(&self, account: &str, secret: &str) -> Result<(), CredError> {
        let entry = keyring::Entry::new("gitcko", account).map_err(|_| CredError::Keychain)?;
        entry.set_password(secret).map_err(|_| CredError::Keychain)
    }
}
