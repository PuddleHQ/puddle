// SPDX-License-Identifier: GPL-3.0-or-later
//! The operating system's own credential store, where the computer has one that works (a Windows
//! runner, a desktop): a value much longer than one entry holds round-trips through
//! [`ChunkedStore`], and what it writes is gone after the delete. A computer with no usable store
//! (a Linux runner without a Secret Service) skips and says so, unless
//! `PUDDLE_REQUIRE_OS_CREDENTIAL_STORE` is set: the Windows job sets it, so a runner without a working
//! store fails the test instead.
//!
//! Every test that writes more than a random-id entry files it under a service name of its own
//! (`KeyringStore::named`) and deletes what it wrote, so a developer's own puddle entries are never
//! read or changed.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code: a panic is how a test fails"
)]

use std::fmt::Write as _;

use puddle_secrets::{
    ChunkedStore, IndexedStore, KeyringStore, Secret, SecretStore, StoredId, Sweep,
    keyring_available,
};

/// A value of `len` characters that is not the same in two places, so a piece in the wrong order
/// reads back as a different value.
fn long_value(len: usize) -> String {
    (0..len)
        .map(|i| char::from(b'a' + u8::try_from((i * 7 + i / 26) % 26).unwrap()))
        .collect()
}

fn fresh_id() -> StoredId {
    let mut bytes = [0_u8; 8];
    getrandom::fill(&mut bytes).unwrap();
    let mut id = String::from("test-");
    for byte in bytes {
        write!(id, "{byte:02x}").unwrap();
    }
    StoredId::new(id).unwrap()
}

/// Skips where the computer has no credential store, and fails there when the CI job that provides
/// one says so.
#[expect(
    clippy::print_stderr,
    reason = "a skip a reader of the test output can see and read the reason for"
)]
fn store_available() -> bool {
    if keyring_available() {
        return true;
    }
    assert!(
        std::env::var_os("PUDDLE_REQUIRE_OS_CREDENTIAL_STORE").is_none(),
        "PUDDLE_REQUIRE_OS_CREDENTIAL_STORE is set but this computer has no credential store that works"
    );
    eprintln!("skipped: this computer has no credential store that works");
    false
}

/// What a test wrote under its own service name, removed when the test ends or panics.
struct Cleanup {
    store: ChunkedStore<KeyringStore>,
    ids: Vec<StoredId>,
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        for id in &self.ids {
            let _ = self.store.delete(id);
        }
    }
}

#[test]
fn the_index_and_the_sweep_work_over_the_real_credential_store() {
    if !store_available() {
        return;
    }
    // A service name no other run or user shares; the index entry lives under it too.
    let service = format!("puddle-test-{}", fresh_id().as_str());
    let raw = KeyringStore::named(&service);
    let index = StoredId::new("puddle-index").unwrap();
    let (kept, orphan, long) = (fresh_id(), fresh_id(), fresh_id());
    let _cleanup = Cleanup {
        store: ChunkedStore::new(raw.clone()),
        ids: vec![kept.clone(), orphan.clone(), long.clone(), index],
    };

    // The shape the product runs: an index over chunking over the platform store.
    let earlier = IndexedStore::new(ChunkedStore::new(raw.clone()));
    earlier.set(&kept, &Secret::new("kept".to_owned())).unwrap();
    earlier
        .set(&orphan, &Secret::new("orphan".to_owned()))
        .unwrap();
    earlier.set(&long, &Secret::new(long_value(5000))).unwrap();
    assert_eq!(
        earlier.get(&long).unwrap().unwrap().expose(),
        long_value(5000)
    );

    // A new process finds what the earlier one listed, and removes what nothing names.
    let later = IndexedStore::new(ChunkedStore::new(raw.clone()));
    let swept = later.sweep_orphans(&|id| *id == kept).unwrap();
    assert_eq!(
        swept,
        Sweep {
            removed: 2,
            stuck: 0
        }
    );
    assert_eq!(later.get(&kept).unwrap().unwrap().expose(), "kept");
    assert!(later.get(&orphan).unwrap().is_none());
    assert!(later.get(&long).unwrap().is_none());
    // With nothing kept any more, the next sweep takes the last one.
    assert_eq!(
        later.sweep_orphans(&|_| false).unwrap(),
        Sweep {
            removed: 1,
            stuck: 0
        }
    );
    assert!(later.get(&kept).unwrap().is_none());
    assert_eq!(later.sweep_orphans(&|_| false).unwrap(), Sweep::default());

    // A delete removes the value and its id from the real index.
    let again = IndexedStore::new(ChunkedStore::new(raw.clone()));
    again.set(&kept, &Secret::new("again".to_owned())).unwrap();
    again.delete(&kept).unwrap();
    let fresh = IndexedStore::new(ChunkedStore::new(raw));
    assert_eq!(fresh.sweep_orphans(&|_| false).unwrap(), Sweep::default());
}

#[test]
fn a_value_longer_than_one_entry_round_trips_through_the_real_credential_store() {
    if !store_available() {
        return;
    }
    let store = ChunkedStore::new(KeyringStore::new());
    let id = fresh_id();
    let long = long_value(5000);

    // The Windows store keeps 2560 bytes per entry: the plain store refuses what chunking stores.
    #[cfg(windows)]
    assert!(
        KeyringStore::new()
            .set(&fresh_id(), &Secret::new(long.clone()))
            .is_err(),
        "the platform limit is gone, so the chunking may be too"
    );

    store.set(&id, &Secret::new(long.clone())).unwrap();
    let read = store.get(&id).unwrap().unwrap();
    assert_eq!(read.expose(), long);
    // A shorter value replaces it in place and leaves no piece behind.
    store.set(&id, &Secret::new("short".to_owned())).unwrap();
    assert_eq!(store.get(&id).unwrap().unwrap().expose(), "short");
    store.set(&id, &Secret::new(long_value(3000))).unwrap();
    assert_eq!(store.get(&id).unwrap().unwrap().expose(), long_value(3000));

    store.delete(&id).unwrap();
    assert!(store.get(&id).unwrap().is_none());
    store.delete(&id).unwrap();
}
