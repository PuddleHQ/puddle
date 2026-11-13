// SPDX-License-Identifier: AGPL-3.0-or-later
//! The OS credential store through the `keyring` crate: Credential Manager on Windows, Keychain
//! on macOS, the Secret Service on Linux.

use zeroize::Zeroize;

use crate::name::StoredId;
use crate::secret::Secret;
use crate::store::{SecretStore, StoreError};

/// Service name every Puddle entry is filed under; the entry's user is `credential:<id>`.
const SERVICE: &str = "puddle";

/// The platform's credential store, under Puddle's own name or, for a test, one of its own.
#[derive(Debug, Default, Clone)]
pub struct KeyringStore {
    /// `None` is Puddle's own [`SERVICE`].
    service: Option<String>,
}

impl KeyringStore {
    /// The store under Puddle's own service name.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The store under `service` instead, so a test that writes real entries (an index, a long
    /// value) never meets the ones the user's Puddle keeps.
    #[must_use]
    pub fn named(service: impl Into<String>) -> Self {
        Self {
            service: Some(service.into()),
        }
    }

    fn entry(&self, id: &StoredId) -> Result<keyring_core::Entry, StoreError> {
        let service = self.service.as_deref().unwrap_or(SERVICE);
        let user = format!("credential:{id}");
        #[cfg(windows)]
        {
            // Credential Manager's default, "enterprise", lets Windows copy the entry to the
            // user's other computers when the account has a roaming profile.
            let modifiers = std::collections::HashMap::from([("persistence", "local")]);
            // The wrapper's constructor sets the default store up and takes no modifiers.
            keyring::Entry::store_status()
                .as_ref()
                .map_err(|_| StoreError)?;
            keyring_core::Entry::new_with_modifiers(service, &user, &modifiers)
                .map_err(|_| StoreError)
        }
        #[cfg(not(windows))]
        {
            keyring::Entry::new(service, &user)
                .map(|entry| entry.inner)
                .map_err(|_| StoreError)
        }
    }
}

impl SecretStore for KeyringStore {
    fn get(&self, id: &StoredId) -> Result<Option<Secret>, StoreError> {
        match self.entry(id)?.get_password() {
            Ok(value) => Ok(Some(Secret::new(value))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(keyring::Error::BadEncoding(mut blob)) => {
                // The error carries the stored bytes; overwrite them before they are freed.
                blob.zeroize();
                Err(StoreError)
            }
            Err(_) => Err(StoreError),
        }
    }

    fn set(&self, id: &StoredId, secret: &Secret) -> Result<(), StoreError> {
        self.entry(id)?
            .set_password(secret.expose())
            .map_err(|_| StoreError)
    }

    fn delete(&self, id: &StoredId) -> Result<(), StoreError> {
        match self.entry(id)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(_) => Err(StoreError),
        }
    }

    /// Windows only: an entry an earlier version wrote as "enterprise" is written again as
    /// "local machine". Other platforms have no such setting.
    fn keep_local(&self, id: &StoredId) -> Result<(), StoreError> {
        #[cfg(windows)]
        {
            let entry = self.entry(id)?;
            match entry.get_attributes() {
                Ok(attributes) if is_kept_local(&attributes) => {}
                Ok(_) => {
                    let value = entry.get_password().map_err(|_| StoreError)?;
                    entry.set_password(&value).map_err(|_| StoreError)?;
                }
                Err(keyring::Error::NoEntry) => {}
                Err(_) => return Err(StoreError),
            }
        }
        let _ = id;
        Ok(())
    }
}

/// Whether the attributes Credential Manager reports for an entry say it stays on this computer.
#[cfg_attr(
    not(any(windows, test)),
    expect(dead_code, reason = "only the Windows keep_local rewrite calls it")
)]
fn is_kept_local(attributes: &std::collections::HashMap<String, String>) -> bool {
    attributes.get("persistence").is_some_and(|p| p == "Local")
}

/// Whether the platform store can be used at all (a Linux host with no Secret Service cannot).
#[must_use]
pub fn keyring_available() -> bool {
    keyring::Entry::store_status().is_ok()
}

#[cfg(test)]
mod tests;
