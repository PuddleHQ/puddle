// SPDX-License-Identifier: GPL-3.0-or-later
//! A store that remembers which ids it holds, so values nothing refers to any more can be found.
//!
//! The OS credential stores cannot list what puddle put in them. A value is left behind when a
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
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::store::MemoryStore;

    fn id(text: &str) -> StoredId {
        StoredId::new(text).unwrap()
    }

    fn removed_one() -> Sweep {
        Sweep {
            removed: 1,
            stuck: 0,
        }
    }

    fn secret(text: &str) -> Secret {
        Secret::new(text.to_owned())
    }

    /// Two handles on one memory store: what a restarted process sees.
    #[derive(Clone, Default)]
    struct Shared(Arc<MemoryStore>);

    impl SecretStore for Shared {
        fn get(&self, id: &StoredId) -> Result<Option<Secret>, StoreError> {
            self.0.get(id)
        }
        fn set(&self, id: &StoredId, secret: &Secret) -> Result<(), StoreError> {
            self.0.set(id, secret)
        }
        fn delete(&self, id: &StoredId) -> Result<(), StoreError> {
            self.0.delete(id)
        }
    }

    #[test]
    fn values_pass_through_and_the_index_follows_them() {
        let disk = Shared::default();
        let store = IndexedStore::new(disk.clone());
        store.set(&id("tok-a"), &secret("one")).unwrap();
        store.set(&id("tok-a"), &secret("two")).unwrap();
        store.set(&id("env-b"), &secret("three")).unwrap();
        assert_eq!(store.get(&id("tok-a")).unwrap().unwrap().expose(), "two");
        assert_eq!(
            disk.get(&id(INDEX_ID)).unwrap().unwrap().expose(),
            "tok-a\nenv-b"
        );
        store.delete(&id("tok-a")).unwrap();
        store.delete(&id("tok-a")).unwrap();
        assert!(store.get(&id("tok-a")).unwrap().is_none());
        assert_eq!(disk.get(&id(INDEX_ID)).unwrap().unwrap().expose(), "env-b");
    }

    #[test]
    fn the_index_is_not_a_credential() {
        let store = IndexedStore::new(MemoryStore::new());
        assert_eq!(store.set(&id(INDEX_ID), &secret("x")), Err(StoreError));
        assert_eq!(store.delete(&id(INDEX_ID)), Err(StoreError));
    }

    #[test]
    fn a_value_left_by_an_earlier_run_goes_when_nothing_refers_to_it() {
        let disk = Shared::default();
        let first = IndexedStore::new(disk.clone());
        first.set(&id("tok-used"), &secret("a")).unwrap();
        first.set(&id("tok-orphan"), &secret("b")).unwrap();
        first.set(&id("env-orphan"), &secret("c")).unwrap();

        let second = IndexedStore::new(disk.clone());
        let removed = second
            .sweep_orphans(&|id| id.as_str() == "tok-used")
            .unwrap();
        assert_eq!(
            removed,
            Sweep {
                removed: 2,
                stuck: 0
            }
        );
        assert!(second.get(&id("tok-used")).unwrap().is_some());
        assert!(second.get(&id("tok-orphan")).unwrap().is_none());
        assert!(second.get(&id("env-orphan")).unwrap().is_none());
        assert_eq!(
            disk.get(&id(INDEX_ID)).unwrap().unwrap().expose(),
            "tok-used"
        );
        assert_eq!(second.sweep_orphans(&|_| false).unwrap(), removed_one());
        assert_eq!(second.sweep_orphans(&|_| false).unwrap(), Sweep::default());
    }

    #[test]
    fn a_value_this_run_wrote_is_not_swept_before_its_row_exists() {
        let store = IndexedStore::new(MemoryStore::new());
        store.set(&id("tok-new"), &secret("a")).unwrap();
        assert_eq!(store.sweep_orphans(&|_| false).unwrap(), Sweep::default());
        assert!(store.get(&id("tok-new")).unwrap().is_some());
    }

    #[test]
    fn a_delete_that_fails_leaves_the_id_listed_for_the_next_sweep() {
        let disk = Shared::default();
        let first = IndexedStore::new(disk.clone());
        first.set(&id("tok-a"), &secret("a")).unwrap();
        disk.0.break_it();
        assert_eq!(first.delete(&id("tok-a")), Err(StoreError));
        disk.0.heal();
        let second = IndexedStore::new(disk.clone());
        assert_eq!(second.sweep_orphans(&|_| false).unwrap(), removed_one());
        assert!(second.get(&id("tok-a")).unwrap().is_none());
    }

    #[test]
    fn a_crash_between_the_value_and_its_row_is_the_same_orphan() {
        // The id is listed first, so a value written and never named is found by the next run;
        // an id listed whose value never arrived costs one delete of nothing.
        let disk = Shared::default();
        let first = IndexedStore::new(disk.clone());
        first.set(&id("tok-crashed"), &secret("a")).unwrap();
        drop(first);
        let second = IndexedStore::new(disk.clone());
        assert_eq!(second.sweep_orphans(&|_| false).unwrap(), removed_one());
        assert!(disk.get(&id("tok-crashed")).unwrap().is_none());
    }

    #[test]
    fn a_store_that_cannot_keep_its_index_refuses_the_value_and_one_that_fails_a_sweep_removes_nothing()
     {
        let disk = Shared::default();
        let store = IndexedStore::new(disk.clone());
        store.set(&id("tok-a"), &secret("a")).unwrap();
        disk.0.break_it();
        assert_eq!(store.set(&id("tok-b"), &secret("b")), Err(StoreError));
        assert_eq!(store.sweep_orphans(&|_| false), Err(StoreError));
        disk.0.heal();
        assert!(store.get(&id("tok-b")).unwrap().is_none());
        let fresh = IndexedStore::new(disk.clone());
        assert_eq!(fresh.sweep_orphans(&|_| true).unwrap(), Sweep::default());
        assert!(fresh.get(&id("tok-a")).unwrap().is_some());
    }

    #[test]
    fn a_damaged_index_line_is_dropped_not_trusted() {
        let disk = Shared::default();
        disk.set(&id(INDEX_ID), &secret("tok-a\nnot an id\n\n../x\ntok-b"))
            .unwrap();
        disk.set(&id("tok-a"), &secret("a")).unwrap();
        let store = IndexedStore::new(disk);
        assert_eq!(
            store.sweep_orphans(&|id| id.as_str() == "tok-b").unwrap(),
            removed_one()
        );
    }

    /// A store that refuses to delete some ids and can lose its index.
    #[derive(Default)]
    struct Fussy {
        inner: MemoryStore,
        stuck: Mutex<HashSet<String>>,
        index_down: Mutex<bool>,
    }

    impl SecretStore for Fussy {
        fn get(&self, id: &StoredId) -> Result<Option<Secret>, StoreError> {
            if id.as_str() == INDEX_ID && *self.index_down.lock().unwrap() {
                return Err(StoreError);
            }
            self.inner.get(id)
        }
        fn set(&self, id: &StoredId, secret: &Secret) -> Result<(), StoreError> {
            self.inner.set(id, secret)
        }
        fn delete(&self, id: &StoredId) -> Result<(), StoreError> {
            if self.stuck.lock().unwrap().contains(id.as_str()) {
                return Err(StoreError);
            }
            self.inner.delete(id)
        }
    }

    #[test]
    fn a_value_that_will_not_go_stays_listed_while_the_others_are_swept() {
        let store = IndexedStore::new(Fussy::default());
        store.set(&id("tok-stuck"), &secret("a")).unwrap();
        store.set(&id("tok-gone"), &secret("b")).unwrap();
        let again = IndexedStore {
            inner: store.inner,
            written: Mutex::new(HashSet::new()),
        };
        again.inner.stuck.lock().unwrap().insert("tok-stuck".into());
        assert_eq!(
            again.sweep_orphans(&|_| false).unwrap(),
            Sweep {
                removed: 1,
                stuck: 1
            }
        );
        assert!(again.get(&id("tok-stuck")).unwrap().is_some());
        assert!(again.get(&id("tok-gone")).unwrap().is_none());
        // Still listed: the next sweep tries it again, and removes it once the store lets it.
        again.inner.stuck.lock().unwrap().clear();
        assert_eq!(again.sweep_orphans(&|_| false).unwrap(), removed_one());
        assert!(again.get(&id("tok-stuck")).unwrap().is_none());
    }

    #[test]
    fn a_delete_goes_through_when_only_the_index_cannot_be_read() {
        let store = IndexedStore::new(Fussy::default());
        store.set(&id("tok-a"), &secret("a")).unwrap();
        *store.inner.index_down.lock().unwrap() = true;
        store.delete(&id("tok-a")).unwrap();
        assert!(store.get(&id("tok-a")).unwrap().is_none());
    }
}
