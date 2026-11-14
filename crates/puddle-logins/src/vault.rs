// SPDX-License-Identifier: AGPL-3.0-or-later
//! Where a workspace's real tokens are kept: one entry in the operating system's credential store
//! for each of the workspace's token slots.
//!
//! The slots of a workspace are a closed, small set (each profile's roles), so an entry's name is
//! worked out from the workspace, the profile and the role, and no index is needed to find the
//! entries of a workspace, to list them or to delete them with it. An entry holds the stand-in
//! the workspace was given and the real token; the stand-in is not a secret (the workspace has
//! it) but is kept beside the token so the pair survives a restart of the workspace, whose disk
//! still holds the stand-in.

use std::fmt::Write as _;
use std::sync::Arc;
use std::time::Duration;

use puddle_secrets::{Secret, SecretStore, StoredId};
use puddle_types::WorkspaceId;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use zeroize::{Zeroize as _, Zeroizing};

use crate::profile::{Profile, Slot};
// The tests below name roles through `super::*`.
#[cfg(test)]
use crate::profile::Role;

/// The version of what an entry holds; a version this build does not know is left alone.
const ENTRY_VERSION: u8 = 1;

/// How long the host waits for the credential store (a locked Secret Service can ask the user to
/// unlock it and wait for the answer).
pub(crate) const STORE_TIMEOUT: Duration = Duration::from_secs(10);

/// Why an entry could not be read or written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum VaultError {
    /// The store did not answer, or refused.
    #[error("the operating system's credential store did not answer")]
    Unavailable,
    /// The entry is not one this build wrote.
    #[error("a saved login could not be understood")]
    Damaged,
}

/// What one entry holds.
#[derive(Deserialize)]
struct Entry {
    v: u8,
    stand_in: String,
    real: String,
}

impl Drop for Entry {
    fn drop(&mut self) {
        self.real.zeroize();
    }
}

#[derive(Serialize)]
struct EntryRef<'a> {
    v: u8,
    stand_in: &'a str,
    real: &'a str,
}

/// A saved pair: the stand-in the workspace holds and the real token behind it.
pub(crate) struct Saved {
    pub(crate) stand_in: String,
    pub(crate) real: Zeroizing<String>,
}

/// The credential store, with Puddle's own entry names.
#[derive(Clone)]
pub(crate) struct Vault {
    store: Arc<dyn SecretStore>,
    workspace: String,
}

impl Vault {
    pub(crate) fn new(store: Arc<dyn SecretStore>, workspace: &WorkspaceId) -> Self {
        Self {
            store,
            workspace: workspace.to_string(),
        }
    }

    fn id(&self, profile: &Profile, slot: Slot) -> Option<StoredId> {
        slot_id(&self.workspace, profile.id, slot)
    }

    /// The saved pair of `profile`'s `role`, or `None` when there is none. Blocking.
    pub(crate) fn read(
        &self,
        profile: &Profile,
        slot: impl Into<Slot>,
    ) -> Result<Option<Saved>, VaultError> {
        let id = self.id(profile, slot.into()).ok_or(VaultError::Damaged)?;
        let Some(secret) = self.store.get(&id).map_err(|_| VaultError::Unavailable)? else {
            return Ok(None);
        };
        let entry: Entry =
            serde_json::from_str(secret.expose()).map_err(|_| VaultError::Damaged)?;
        if entry.v != ENTRY_VERSION {
            return Err(VaultError::Damaged);
        }
        Ok(Some(Saved {
            stand_in: entry.stand_in.clone(),
            real: Zeroizing::new(entry.real.clone()),
        }))
    }

    /// Whether an entry for `profile`'s `role` exists, without reading its token into memory.
    /// Blocking.
    pub(crate) fn holds(
        &self,
        profile: &Profile,
        slot: impl Into<Slot>,
    ) -> Result<bool, VaultError> {
        let id = self.id(profile, slot.into()).ok_or(VaultError::Damaged)?;
        self.store
            .get(&id)
            .map(|found| found.is_some())
            .map_err(|_| VaultError::Unavailable)
    }

    /// Saves the pair of `profile`'s `role`, replacing the earlier one. Blocking.
    pub(crate) fn write(
        &self,
        profile: &Profile,
        slot: impl Into<Slot>,
        stand_in: &str,
        real: &str,
    ) -> Result<(), VaultError> {
        let id = self.id(profile, slot.into()).ok_or(VaultError::Damaged)?;
        let text = Zeroizing::new(
            serde_json::to_string(&EntryRef {
                v: ENTRY_VERSION,
                stand_in,
                real,
            })
            .map_err(|_| VaultError::Damaged)?,
        );
        self.store
            .set(&id, &Secret::new((*text).clone()))
            .map_err(|_| VaultError::Unavailable)
    }

    /// Deletes the pair of `profile`'s `role`; one that is not there is fine. Blocking.
    pub(crate) fn delete(
        &self,
        profile: &Profile,
        slot: impl Into<Slot>,
    ) -> Result<(), VaultError> {
        let id = self.id(profile, slot.into()).ok_or(VaultError::Damaged)?;
        self.store.delete(&id).map_err(|_| VaultError::Unavailable)
    }
}

/// The name of the entry for one slot: `login-<16 hex of the workspace's hash>-<profile>-<role>`,
/// with `-<resource>` after the role for a slot that has one (a slot without keeps the name it
/// always had). The workspace is hashed because its name and the rest may not fit the store's 64
/// characters.
pub(crate) fn slot_id(workspace: &str, profile: &str, slot: impl Into<Slot>) -> Option<StoredId> {
    let hex = workspace_hash(workspace);
    StoredId::new(format!("login-{hex}-{profile}-{}", slot.into().key())).ok()
}

/// 16 hex characters of the hash of a workspace's name: what names its entries and its marker.
pub(crate) fn workspace_hash(workspace: &str) -> String {
    let digest = Sha256::digest(workspace.as_bytes());
    let mut hex = String::with_capacity(16);
    for byte in digest.iter().take(8) {
        // writing into a `String` cannot fail
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

#[cfg(test)]
mod tests;
