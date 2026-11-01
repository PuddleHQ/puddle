// SPDX-License-Identifier: GPL-3.0-or-later
//! The OS credential store through the `keyring` crate: Credential Manager on Windows, Keychain
//! on macOS, the Secret Service on Linux.

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

    fn entry(&self, id: &StoredId) -> Result<keyring::Entry, StoreError> {
        keyring::Entry::new(
            self.service.as_deref().unwrap_or(SERVICE),
            &format!("credential:{id}"),
        )
        .map_err(|_| StoreError)
    }
}

impl SecretStore for KeyringStore {
    fn get(&self, id: &StoredId) -> Result<Option<Secret>, StoreError> {
        match self.entry(id)?.get_password() {
            Ok(value) => Ok(Some(Secret::new(value))),
            Err(keyring::Error::NoEntry) => Ok(None),
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
}

/// Whether the platform store can be used at all (a Linux host with no Secret Service cannot).
#[must_use]
pub fn keyring_available() -> bool {
    keyring::Entry::store_status().is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id() -> StoredId {
        StoredId::new("probe").unwrap()
    }

    #[test]
    fn the_store_is_filed_under_puddle_unless_a_test_names_another_service() {
        assert_eq!(KeyringStore::new().service, None);
        assert_eq!(
            KeyringStore::named("puddle-test-x").service.as_deref(),
            Some("puddle-test-x")
        );
    }

    #[test]
    fn availability_is_what_the_platform_store_reports() {
        assert_eq!(keyring_available(), keyring::Entry::store_status().is_ok());
    }

    #[test]
    fn a_computer_without_a_credential_store_fails_every_call_and_loses_nothing_silently() {
        if !keyring_available() {
            let store = KeyringStore::named("puddle-test-unavailable");
            assert_eq!(store.get(&id()).err(), Some(StoreError));
            assert_eq!(
                store.set(&id(), &Secret::new("v".to_owned())),
                Err(StoreError)
            );
            assert_eq!(store.delete(&id()), Err(StoreError));
        }
    }
}
