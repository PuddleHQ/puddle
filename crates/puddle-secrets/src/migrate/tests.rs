// SPDX-License-Identifier: AGPL-3.0-or-later
use std::cell::Cell;
use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex, PoisonError};

use super::*;
use crate::file_vault::{FileVault, KeySourceTag, VaultKey};
use crate::indexed::IndexedStore;
use crate::store::{MemoryStore, StoreError};

fn id(text: &str) -> StoredId {
    StoredId::new(text).unwrap()
}

fn secret(text: &str) -> Secret {
    Secret::new(text.to_owned())
}

fn value_of(store: &dyn SecretStore, name: &str) -> Option<String> {
    store.get(&id(name)).unwrap().map(|s| s.expose().to_owned())
}

fn ids() -> Vec<StoredId> {
    ["env-a", "tok-b", "env-c"].map(id).to_vec()
}

fn fill(store: &dyn SecretStore) {
    for (name, value) in [
        ("env-a", "one"),
        ("tok-b", "two \u{1f986} unicode"),
        ("env-c", "three"),
    ] {
        store.set(&id(name), &secret(value)).unwrap();
    }
}

fn file_vault(dir: &tempfile::TempDir) -> IndexedStore<FileVault> {
    IndexedStore::new(
        FileVault::create(
            &dir.path().join("vault"),
            VaultKey::from_bytes([7; 32]),
            KeySourceTag::KeyFile,
        )
        .unwrap(),
    )
}

#[test]
fn every_value_arrives_both_ways_over_a_memory_store_and_a_file_vault() {
    let dir = tempfile::tempdir().unwrap();
    let memory = IndexedStore::new(MemoryStore::new());
    let vault = file_vault(&dir);
    fill(&memory);

    let there = move_values(&memory, &vault, &ids(), &mut || Ok(())).unwrap();
    assert_eq!(there, Moved { copied: 3, left: 0 });
    for (name, value) in [
        ("env-a", "one"),
        ("tok-b", "two \u{1f986} unicode"),
        ("env-c", "three"),
    ] {
        assert_eq!(value_of(&vault, name).as_deref(), Some(value));
        assert_eq!(value_of(&memory, name), None, "{name} left the old store");
    }
    assert_eq!(memory.listed_ids().unwrap(), Vec::new());
    assert_eq!(vault.listed_ids().unwrap().len(), 3);

    let back = move_values(&vault, &memory, &ids(), &mut || Ok(())).unwrap();
    assert_eq!(back, Moved { copied: 3, left: 0 });
    assert_eq!(
        value_of(&memory, "tok-b").as_deref(),
        Some("two \u{1f986} unicode")
    );
    assert_eq!(value_of(&vault, "tok-b"), None);
}

#[test]
fn an_id_without_a_value_and_a_repeated_id_are_skipped_not_counted() {
    let old = MemoryStore::new();
    let new = MemoryStore::new();
    old.set(&id("env-a"), &secret("one")).unwrap();
    let asked = vec![id("env-a"), id("env-a"), id("gone")];
    let moved = move_values(&old, &new, &asked, &mut || Ok(())).unwrap();
    assert_eq!(moved, Moved { copied: 1, left: 0 });
    assert_eq!(new.ids(), vec!["env-a".to_owned()]);
}

/// A store that refuses or garbles what it is given.
struct Hostile {
    inner: MemoryStore,
    refuse: Option<&'static str>,
    garble: Option<&'static str>,
}

impl SecretStore for Hostile {
    fn get(&self, id: &StoredId) -> Result<Option<Secret>, StoreError> {
        self.inner.get(id)
    }
    fn set(&self, id: &StoredId, value: &Secret) -> Result<(), StoreError> {
        if self.refuse == Some(id.as_str()) {
            return Err(StoreError);
        }
        if self.garble == Some(id.as_str()) {
            return self.inner.set(id, &secret("garbled"));
        }
        self.inner.set(id, value)
    }
    fn delete(&self, id: &StoredId) -> Result<(), StoreError> {
        self.inner.delete(id)
    }
}

#[test]
fn a_value_the_target_refuses_or_garbles_leaves_the_old_store_in_charge_and_no_copies() {
    for (refuse, garble) in [(Some("tok-b"), None), (None, Some("env-c"))] {
        let old = MemoryStore::new();
        fill(&old);
        let new = Hostile {
            inner: MemoryStore::new(),
            refuse,
            garble,
        };
        let switched = Cell::new(false);
        let err = move_values(&old, &new, &ids(), &mut || {
            switched.set(true);
            Ok(())
        })
        .unwrap_err();
        assert_eq!(err, MigrateError::Target);
        assert!(!switched.get(), "the record is not written");
        assert_eq!(old.ids().len(), 3, "the old store keeps every value");
        assert_eq!(
            new.inner.ids(),
            Vec::<String>::new(),
            "the copies are removed again"
        );
    }
}

#[test]
fn a_source_that_cannot_be_read_changes_nothing() {
    let old = MemoryStore::new();
    fill(&old);
    let new = MemoryStore::new();
    old.break_it();
    let switched = Cell::new(false);
    let err = move_values(&old, &new, &ids(), &mut || {
        switched.set(true);
        Ok(())
    })
    .unwrap_err();
    assert_eq!(err, MigrateError::Source);
    assert!(!switched.get());
    old.heal();
    assert_eq!(old.ids().len(), 3);
    assert_eq!(new.ids(), Vec::<String>::new());
}

#[test]
fn a_record_that_cannot_be_written_removes_the_copies() {
    let old = MemoryStore::new();
    fill(&old);
    let new = MemoryStore::new();
    let err = move_values(&old, &new, &ids(), &mut || Err(RecordError::Io)).unwrap_err();
    assert_eq!(err, MigrateError::Record);
    assert_eq!(old.ids().len(), 3);
    assert_eq!(new.ids(), Vec::<String>::new());
}

/// Which call of a store a test makes "the process dies" at.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Op {
    Get,
    Set,
    Delete,
}

/// A store that panics at the n-th call of one kind, before doing it: the process is gone.
struct Mortal {
    inner: Arc<MemoryStore>,
    dies_at: Option<(Op, usize)>,
    calls: Mutex<HashMap<Op, usize>>,
}

impl Mortal {
    fn new(inner: &Arc<MemoryStore>, dies_at: Option<(Op, usize)>) -> Self {
        Self {
            inner: inner.clone(),
            dies_at,
            calls: Mutex::default(),
        }
    }

    fn call(&self, op: Op) {
        let mut calls = self.calls.lock().unwrap_or_else(PoisonError::into_inner);
        let n = calls.entry(op).or_default();
        *n += 1;
        assert!(
            self.dies_at != Some((op, *n)),
            "the process died at {op:?} {n}"
        );
    }
}

impl SecretStore for Mortal {
    fn get(&self, id: &StoredId) -> Result<Option<Secret>, StoreError> {
        self.call(Op::Get);
        self.inner.get(id)
    }
    fn set(&self, id: &StoredId, value: &Secret) -> Result<(), StoreError> {
        self.call(Op::Set);
        self.inner.set(id, value)
    }
    fn delete(&self, id: &StoredId) -> Result<(), StoreError> {
        self.call(Op::Delete);
        self.inner.delete(id)
    }
}

/// The record of a run: which store is in charge.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Charge {
    Old,
    New,
}

#[test]
fn a_crash_at_each_step_ends_in_one_consistent_store_after_a_rerun() {
    // (name, the call that dies, how far the record closure gets, charge after the crash)
    // The new store's `get` is the first verify read; the old store's `get`s are the 3 reads.
    let cases: [(&str, Op, usize, Option<usize>, Charge); 6] = [
        ("after the reads", Op::Get, 3, None, Charge::Old),
        ("during the copy", Op::Set, 2, None, Charge::Old),
        ("after the copy", Op::Get, 1, None, Charge::Old),
        ("before the switch", Op::Get, 99, Some(0), Charge::Old),
        ("after the switch", Op::Get, 99, Some(1), Charge::New),
        ("in the deletes", Op::Delete, 2, None, Charge::New),
    ];
    for (name, op, nth, record_step, charge_after) in cases {
        let old = Arc::new(MemoryStore::new());
        fill(old.as_ref());
        let new = Arc::new(MemoryStore::new());
        let charge = Cell::new(Charge::Old);
        // The store that dies: the old store for reads and deletes, the new one for writes
        // and verifying reads.
        let (old_dies, new_dies) = match (op, name) {
            (Op::Get, "after the reads") | (Op::Delete, _) => (Some((op, nth)), None),
            (Op::Get | Op::Set, _) if record_step.is_none() => (None, Some((op, nth))),
            _ => (None, None),
        };
        let crashed = catch_unwind(AssertUnwindSafe(|| {
            move_values(
                &Mortal::new(&old, old_dies),
                &Mortal::new(&new, new_dies),
                &ids(),
                &mut || {
                    assert!(record_step != Some(0), "the process died before the switch");
                    charge.set(Charge::New);
                    assert!(record_step != Some(1), "the process died after the switch");
                    Ok(())
                },
            )
        }));
        assert!(crashed.is_err(), "{name}: the run must have crashed");
        assert_eq!(charge.get(), charge_after, "{name}");

        // The rerun, as the host does it: the record decides.
        match charge.get() {
            Charge::Old => {
                let moved = move_values(old.as_ref(), new.as_ref(), &ids(), &mut || {
                    charge.set(Charge::New);
                    Ok(())
                })
                .unwrap();
                assert_eq!(moved.copied, 3, "{name}");
            }
            Charge::New => {
                // The values the crash left in the old store are removed; the ones the new store
                // is missing cannot be (the switch happened after every copy was checked).
                let cleared = clear_old_store(old.as_ref(), &ids());
                assert_eq!(cleared.left, 0, "{name}");
            }
        }
        assert_eq!(charge.get(), Charge::New, "{name}");
        assert_eq!(
            old.ids(),
            Vec::<String>::new(),
            "{name}: nothing is left in the old store"
        );
        assert_eq!(new.ids(), vec!["env-a", "env-c", "tok-b"], "{name}");
        assert_eq!(
            value_of(new.as_ref(), "env-a").as_deref(),
            Some("one"),
            "{name}"
        );
        assert_eq!(
            value_of(new.as_ref(), "env-c").as_deref(),
            Some("three"),
            "{name}"
        );
    }
}

#[test]
fn finishing_the_deletes_keeps_the_newer_value_of_the_store_in_charge() {
    let old = MemoryStore::new();
    let new = MemoryStore::new();
    old.set(&id("env-a"), &secret("stale")).unwrap();
    old.set(&id("env-c"), &secret("stale")).unwrap();
    new.set(&id("env-a"), &secret("newer")).unwrap();
    let cleared = clear_old_store(&old, &[id("env-a"), id("env-c"), id("never-there")]);
    assert_eq!(
        cleared,
        Cleared {
            removed: 2,
            left: 0
        }
    );
    assert_eq!(old.ids(), Vec::<String>::new());
    assert_eq!(value_of(&new, "env-a").as_deref(), Some("newer"));
    assert_eq!(new.ids().len(), 1, "nothing was copied over");
}

#[test]
fn a_store_that_will_not_give_up_a_value_is_counted_not_hidden() {
    let old = MemoryStore::new();
    fill(&old);
    old.break_it();
    let cleared = clear_old_store(&old, &ids());
    assert_eq!(
        cleared,
        Cleared {
            removed: 0,
            left: 3
        }
    );
}

/// Reads work, deletes do not: the old store after the switch.
struct KeepsEverything(MemoryStore);

impl SecretStore for KeepsEverything {
    fn get(&self, id: &StoredId) -> Result<Option<Secret>, StoreError> {
        self.0.get(id)
    }
    fn set(&self, id: &StoredId, value: &Secret) -> Result<(), StoreError> {
        self.0.set(id, value)
    }
    fn delete(&self, _: &StoredId) -> Result<(), StoreError> {
        Err(StoreError)
    }
}

#[test]
fn deletes_that_fail_after_the_switch_are_counted_and_the_move_still_succeeds() {
    let old = KeepsEverything(MemoryStore::new());
    fill(&old);
    let new = MemoryStore::new();
    let moved = move_values(&old, &new, &ids(), &mut || Ok(())).unwrap();
    assert_eq!(moved, Moved { copied: 3, left: 3 });
    assert_eq!(new.ids().len(), 3);
    let cleared = clear_old_store(&old, &ids());
    assert_eq!(
        cleared,
        Cleared {
            removed: 0,
            left: 3
        }
    );
}

#[test]
fn the_error_texts_name_no_value_and_no_id() {
    for err in [
        MigrateError::Source,
        MigrateError::Target,
        MigrateError::Record,
    ] {
        let text = err.to_string();
        assert!(text.contains("old store"), "{text}");
    }
}
