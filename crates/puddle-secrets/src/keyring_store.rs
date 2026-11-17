// SPDX-License-Identifier: AGPL-3.0-or-later
//! The OS credential store through the `keyring` crate: Credential Manager on Windows, Keychain
//! on macOS, the Secret Service on Linux.

use zeroize::Zeroize;

use crate::name::StoredId;
use crate::os_store::ensure_os_store;
use crate::secret::Secret;
use crate::store::{SecretStore, StoreError};

/// Service name every Puddle entry is filed under; the entry's user is `credential:<id>`.
const SERVICE: &str = "puddle";

#[cfg(windows)]
mod windows;

/// Added to an entry's user for the copy `keep_local` holds while it rewrites the entry.
#[cfg(windows)]
const BACKUP_SUFFIX: &str = ".keep-local";

/// Runs one call to the platform store. On Windows it waits for every other Puddle process's call
/// to end first: Credential Manager loses changes made side by side (see `windows.rs`).
fn one_call<T>(call: impl FnOnce() -> T) -> T {
    #[cfg(windows)]
    let _held = windows::CredentialLock::hold();
    call()
}

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
        self.entry_for_user(&format!("credential:{id}"))
    }

    fn entry_for_user(&self, user: &str) -> Result<keyring_core::Entry, StoreError> {
        let service = self.service.as_deref().unwrap_or(SERVICE);
        #[cfg(windows)]
        {
            // Credential Manager's default, "enterprise", lets Windows copy the entry to the
            // user's other computers when the account has a roaming profile.
            let modifiers = std::collections::HashMap::from([("persistence", "local")]);
            ensure_os_store()?;
            keyring_core::Entry::new_with_modifiers(service, user, &modifiers)
                .map_err(|_| StoreError)
        }
        #[cfg(not(windows))]
        {
            ensure_os_store()?;
            keyring_core::Entry::new(service, user).map_err(|_| StoreError)
        }
    }
}

impl SecretStore for KeyringStore {
    fn get(&self, id: &StoredId) -> Result<Option<Secret>, StoreError> {
        let entry = self.entry(id)?;
        match one_call(|| entry.get_password()) {
            Ok(value) => Ok(Some(Secret::new(value))),
            Err(keyring_core::Error::NoEntry) => Ok(None),
            Err(keyring_core::Error::BadEncoding(mut blob)) => {
                // The error carries the stored bytes; overwrite them before they are freed.
                blob.zeroize();
                Err(StoreError)
            }
            Err(_) => Err(StoreError),
        }
    }

    fn set(&self, id: &StoredId, secret: &Secret) -> Result<(), StoreError> {
        let entry = self.entry(id)?;
        one_call(|| entry.set_password(secret.expose())).map_err(|_| StoreError)
    }

    fn delete(&self, id: &StoredId) -> Result<(), StoreError> {
        let entry = self.entry(id)?;
        match one_call(|| entry.delete_credential()) {
            Ok(()) | Err(keyring_core::Error::NoEntry) => Ok(()),
            Err(_) => Err(StoreError),
        }
    }

    /// Windows only: an entry an earlier version wrote as "enterprise" is written again as
    /// "local machine". Other platforms have no such setting.
    ///
    /// Writing the local entry over the enterprise one leaves the enterprise copy behind: a delete
    /// removes only the local one and the copy comes back a moment later, still roaming. So the
    /// enterprise entry is deleted first. The value waits in a local backup entry meanwhile, and
    /// a run that finds the entry missing and the backup there (a crash in between) restores it.
    fn keep_local(&self, id: &StoredId) -> Result<(), StoreError> {
        #[cfg(windows)]
        {
            let entry = self.entry(id)?;
            let backup = self.entry_for_user(&format!("credential:{id}{BACKUP_SUFFIX}"))?;
            one_call(|| match entry.get_attributes() {
                Ok(attributes) if is_kept_local(&attributes) => Ok(()),
                Ok(_) => {
                    let value = Secret::new(entry.get_password().map_err(|_| StoreError)?);
                    backup
                        .set_password(value.expose())
                        .map_err(|_| StoreError)?;
                    entry.delete_credential().map_err(|_| StoreError)?;
                    entry.set_password(value.expose()).map_err(|_| StoreError)?;
                    // A backup that cannot be removed is harmless: it is replaced next time.
                    // The value is already in the local entry, so the failure changes nothing.
                    let _ = backup.delete_credential();
                    Ok(())
                }
                Err(keyring_core::Error::NoEntry) => match backup.get_password() {
                    Ok(value) => {
                        let value = Secret::new(value);
                        entry.set_password(value.expose()).map_err(|_| StoreError)?;
                        // Same as above: the restored entry is what counts, a leftover backup is replaced next time.
                        let _ = backup.delete_credential();
                        Ok(())
                    }
                    Err(keyring_core::Error::NoEntry) => Ok(()),
                    Err(_) => Err(StoreError),
                },
                Err(_) => Err(StoreError),
            })?;
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

/// Whether the platform store's service answers (a Linux host with no Secret Service does not).
/// A locked Secret Service answers and still refuses every write: use [`crate::probe_os_store`]
/// to know whether a value can be stored.
#[must_use]
pub fn keyring_available() -> bool {
    ensure_os_store().is_ok()
}

#[cfg(test)]
mod tests;
