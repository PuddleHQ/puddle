// SPDX-License-Identifier: AGPL-3.0-or-later
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

/// A store whose index entry can be made read-only.
#[derive(Default)]
struct IndexLocked {
    inner: MemoryStore,
    locked: Mutex<bool>,
}

impl SecretStore for IndexLocked {
    fn get(&self, id: &StoredId) -> Result<Option<Secret>, StoreError> {
        self.inner.get(id)
    }
    fn set(&self, id: &StoredId, secret: &Secret) -> Result<(), StoreError> {
        if id.as_str() == INDEX_ID && *self.locked.lock().unwrap() {
            return Err(StoreError);
        }
        self.inner.set(id, secret)
    }
    fn delete(&self, id: &StoredId) -> Result<(), StoreError> {
        self.inner.delete(id)
    }
}

#[test]
fn a_sweep_that_removes_nothing_does_not_write_the_index() {
    let store = IndexedStore::new(IndexLocked::default());
    store.set(&id("tok-a"), &secret("a")).unwrap();
    let later = IndexedStore {
        inner: store.inner,
        written: Mutex::new(HashSet::new()),
    };
    *later.inner.locked.lock().unwrap() = true;
    // Everything is kept: nothing changes, so there is nothing to write (and no failure).
    assert_eq!(later.sweep_orphans(&|_| true).unwrap(), Sweep::default());
    // A removal does need the index written, and says so when it cannot.
    assert_eq!(later.sweep_orphans(&|_| false), Err(StoreError));
}

#[test]
fn keeping_all_local_reaches_every_listed_id_and_the_index() {
    let store = IndexedStore::new(MemoryStore::new());
    store.set(&id("tok-a"), &secret("a")).unwrap();
    store.set(&id("tok-b"), &secret("b")).unwrap();
    assert_eq!(store.keep_all_local(), Ok(0));
    assert_eq!(store.inner.kept_local(), ["tok-a", "tok-b", "puddle-index"]);
}

#[test]
fn keeping_one_entry_local_reaches_the_store_underneath() {
    let store = IndexedStore::new(MemoryStore::new());
    store.keep_local(&id("tok-a")).unwrap();
    assert_eq!(store.inner.kept_local(), ["tok-a"]);
}

/// A store that holds values but cannot rewrite any of them.
#[derive(Default)]
struct NoRewrite(MemoryStore);

impl SecretStore for NoRewrite {
    fn get(&self, id: &StoredId) -> Result<Option<Secret>, StoreError> {
        self.0.get(id)
    }
    fn set(&self, id: &StoredId, secret: &Secret) -> Result<(), StoreError> {
        self.0.set(id, secret)
    }
    fn delete(&self, id: &StoredId) -> Result<(), StoreError> {
        self.0.delete(id)
    }
    fn keep_local(&self, _: &StoredId) -> Result<(), StoreError> {
        Err(StoreError)
    }
}

#[test]
fn entries_that_cannot_be_rewritten_are_counted_not_fatal() {
    let store = IndexedStore::new(NoRewrite::default());
    store.set(&id("tok-a"), &secret("a")).unwrap();
    // The listed id and the index itself.
    assert_eq!(store.keep_all_local(), Ok(2));
    store.delete(&id("tok-a")).unwrap();
}
