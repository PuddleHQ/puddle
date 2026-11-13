// SPDX-License-Identifier: AGPL-3.0-or-later
//! Where a pasted token is kept: the seam over the OS credential store.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use crate::name::StoredId;
use crate::secret::Secret;

/// The store did not do what was asked. Carries no detail: a store's own message could quote a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the OS credential store failed")]
pub struct StoreError;

/// A place for pasted tokens, addressed by [`StoredId`]. Blocking: callers run it off the async
/// threads. The product implementation is [`KeyringStore`](crate::KeyringStore); tests use
/// [`MemoryStore`].
pub trait SecretStore: Send + Sync {
    /// The token under `id`, or `None` when there is none.
    ///
    /// # Errors
    /// [`StoreError`] when the store cannot be reached.
    fn get(&self, id: &StoredId) -> Result<Option<Secret>, StoreError>;

    /// Stores `secret` under `id`, replacing any earlier one.
    ///
    /// # Errors
    /// [`StoreError`] when the store cannot be reached or refuses.
    fn set(&self, id: &StoredId, secret: &Secret) -> Result<(), StoreError>;

    /// Removes the token under `id`; a missing one is fine.
    ///
    /// # Errors
    /// [`StoreError`] when the store cannot be reached.
    fn delete(&self, id: &StoredId) -> Result<(), StoreError>;

    /// Makes the entry under `id` one that stays on this computer. The Windows store writes
    /// "enterprise" entries, which a roaming profile copies to the user's other computers, unless
    /// told otherwise; an entry an older version wrote is rewritten here. A store with no such
    /// setting, or no entry under `id`, does nothing.
    ///
    /// # Errors
    /// [`StoreError`] when the store cannot be reached.
    fn keep_local(&self, id: &StoredId) -> Result<(), StoreError> {
        let _ = id;
        Ok(())
    }

    /// [`Self::keep_local`] for every value this store knows it holds, and says how many it
    /// could not rewrite. A store that does not know what it holds does nothing.
    ///
    /// # Errors
    /// [`StoreError`] when the store cannot be reached.
    fn keep_all_local(&self) -> Result<usize, StoreError> {
        Ok(0)
    }

    /// Removes the values this store holds that `keep` refuses, and says how many went and how
    /// many it tried and could not remove. A store that does not know what it holds (it has no
    /// index) has nothing to sweep.
    ///
    /// # Errors
    /// [`StoreError`] when the store cannot be reached; nothing is removed then.
    fn sweep_orphans(&self, keep: &dyn Fn(&StoredId) -> bool) -> Result<Sweep, StoreError> {
        let _ = keep;
        Ok(Sweep::default())
    }
}

/// What [`SecretStore::sweep_orphans`] did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Sweep {
    /// Values removed.
    pub removed: usize,
    /// Values the sweep tried to remove and the store would not give up. They stay listed, so the
    /// next sweep tries again.
    pub stuck: usize,
}

/// A store in memory: nothing persists, nothing leaves the process.
#[derive(Default)]
pub struct MemoryStore {
    entries: Mutex<HashMap<String, String>>,
    kept_local: Mutex<Vec<String>>,
    broken: Mutex<bool>,
}

impl MemoryStore {
    /// An empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Makes every later call fail, to test the unavailable-store path.
    pub fn break_it(&self) {
        *self.broken.lock().unwrap_or_else(PoisonError::into_inner) = true;
    }

    /// Makes the calls work again after [`Self::break_it`].
    pub fn heal(&self) {
        *self.broken.lock().unwrap_or_else(PoisonError::into_inner) = false;
    }

    /// The ids held, sorted: for tests that look for what a caller left behind.
    #[must_use]
    pub fn ids(&self) -> Vec<String> {
        let entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        let mut ids: Vec<String> = entries.keys().cloned().collect();
        ids.sort();
        ids
    }

    /// The ids [`SecretStore::keep_local`] was asked about, in order: for tests of the callers.
    #[must_use]
    pub fn kept_local(&self) -> Vec<String> {
        self.kept_local
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn check(&self) -> Result<(), StoreError> {
        if *self.broken.lock().unwrap_or_else(PoisonError::into_inner) {
            return Err(StoreError);
        }
        Ok(())
    }
}

impl SecretStore for MemoryStore {
    fn get(&self, id: &StoredId) -> Result<Option<Secret>, StoreError> {
        self.check()?;
        let entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        Ok(entries.get(id.as_str()).map(|v| Secret::new(v.clone())))
    }

    fn set(&self, id: &StoredId, secret: &Secret) -> Result<(), StoreError> {
        self.check()?;
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        entries.insert(id.as_str().to_owned(), secret.expose().to_owned());
        Ok(())
    }

    fn delete(&self, id: &StoredId) -> Result<(), StoreError> {
        self.check()?;
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        entries.remove(id.as_str());
        Ok(())
    }

    fn keep_local(&self, id: &StoredId) -> Result<(), StoreError> {
        self.check()?;
        self.kept_local
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(id.as_str().to_owned());
        Ok(())
    }
}

#[cfg(test)]
mod tests;
