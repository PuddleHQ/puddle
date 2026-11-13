// SPDX-License-Identifier: AGPL-3.0-or-later
//! A store that remembers which ids it holds, so values nothing refers to any more can be found.
//!
//! The OS credential stores cannot list what Puddle put in them. A value is left behind when a
//! delete fails, or when the process stops between writing the value and writing the row that
//! names it, and without a list nobody ever removes it. [`IndexedStore`] keeps that list as one
//! more entry in the store underneath: an id is written to it before its value, and taken out
//! only after the value is gone, so a crash at any point leaves the id listed.
//! [`SecretStore::sweep_orphans`] then removes what the caller no longer refers to.
//!
//! Values written before the index existed are not in it and are never swept.

use std::collections::HashSet;
use std::sync::{Mutex, PoisonError};

use crate::name::StoredId;
use crate::secret::Secret;
use crate::store::{SecretStore, StoreError, Sweep};

/// The id the list is kept under. It is not a credential and is never offered as one.
const INDEX_ID: &str = "puddle-index";

/// Wraps the store underneath.
#[derive(Debug, Default)]
pub struct IndexedStore<S> {
    inner: S,
    /// The ids this process wrote, and the lock that keeps one change of the list at a time. A
    /// sweep leaves those ids alone: the row that names a value comes after the value, so a value
    /// written a moment ago may be about to be named.
    written: Mutex<HashSet<StoredId>>,
}

impl<S: SecretStore> IndexedStore<S> {
    /// A store over `inner`.
    #[must_use]
    pub fn new(inner: S) -> Self {
        Self {
            inner,
            written: Mutex::new(HashSet::new()),
        }
    }

    fn index_id() -> Result<StoredId, StoreError> {
        StoredId::new(INDEX_ID).map_err(|_| StoreError)
    }

    /// The listed ids, in order. A line that is not an id is dropped.
    fn listed(&self) -> Result<Vec<StoredId>, StoreError> {
        let Some(text) = self.inner.get(&Self::index_id()?)? else {
            return Ok(Vec::new());
        };
        Ok(text
            .expose()
            .lines()
            .filter_map(|line| StoredId::new(line).ok())
            .collect())
    }

    fn write_list(&self, ids: &[StoredId]) -> Result<(), StoreError> {
        let text = ids
            .iter()
            .map(StoredId::as_str)
            .collect::<Vec<_>>()
            .join("\n");
        self.inner.set(&Self::index_id()?, &Secret::new(text))
    }
}

impl<S: SecretStore> SecretStore for IndexedStore<S> {
    fn get(&self, id: &StoredId) -> Result<Option<Secret>, StoreError> {
        self.inner.get(id)
    }

    fn set(&self, id: &StoredId, secret: &Secret) -> Result<(), StoreError> {
        if id.as_str() == INDEX_ID {
            return Err(StoreError);
        }
        let mut written = self.written.lock().unwrap_or_else(PoisonError::into_inner);
        let mut ids = self.listed()?;
        if !ids.contains(id) {
            ids.push(id.clone());
            // A value nobody can find again is worse than one refused now.
            self.write_list(&ids)?;
        }
        written.insert(id.clone());
        self.inner.set(id, secret)
    }

    fn delete(&self, id: &StoredId) -> Result<(), StoreError> {
        if id.as_str() == INDEX_ID {
            return Err(StoreError);
        }
        let _guard = self.written.lock().unwrap_or_else(PoisonError::into_inner);
        self.inner.delete(id)?;
        // The value is gone. A list that cannot be changed now only keeps an id that a sweep
        // finds nothing under.
        if let Ok(mut ids) = self.listed() {
            ids.retain(|listed| listed != id);
            let _ = self.write_list(&ids);
        }
        Ok(())
    }

    fn keep_local(&self, id: &StoredId) -> Result<(), StoreError> {
        self.inner.keep_local(id)
    }

    fn keep_all_local(&self) -> Result<usize, StoreError> {
        let mut failed = 0;
        let index = Self::index_id()?;
        for id in self.listed()?.iter().chain(std::iter::once(&index)) {
            if self.inner.keep_local(id).is_err() {
                failed += 1;
            }
        }
        Ok(failed)
    }

    fn sweep_orphans(&self, keep: &dyn Fn(&StoredId) -> bool) -> Result<Sweep, StoreError> {
        let written = self.written.lock().unwrap_or_else(PoisonError::into_inner);
        let mut left = Vec::new();
        let mut sweep = Sweep::default();
        for id in self.listed()? {
            if written.contains(&id) || keep(&id) {
                left.push(id);
            } else if self.inner.delete(&id).is_ok() {
                sweep.removed += 1;
            } else {
                sweep.stuck += 1;
                left.push(id);
            }
        }
        if sweep.removed > 0 {
            self.write_list(&left)?;
        }
        Ok(sweep)
    }
}

#[cfg(test)]
mod tests;
