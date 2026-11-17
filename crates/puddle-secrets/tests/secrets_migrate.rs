// SPDX-License-Identifier: GPL-3.0-or-later
//! `puddle secrets migrate` over the operating system's credential store and the file vault, in
//! both directions, with values of the sizes and kinds the keyring has trouble with. A computer
//! without a credential store that works skips the run and says so, unless
//! `PUDDLE_REQUIRE_OS_CREDENTIAL_STORE` is set (the CI job that starts gnome-keyring sets it).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code: a panic is how a test fails"
)]

use std::fmt::Write as _;

use puddle_secrets::{
    ChunkedStore, FileVault, IndexedStore, KeySourceTag, KeyringStore, Secret, SecretStore,
    StoredId, VaultKey, clear_old_store, keyring_available, move_values,
};

fn random_hex() -> String {
    let mut bytes = [0_u8; 8];
    getrandom::fill(&mut bytes).unwrap();
    bytes.iter().fold(String::new(), |mut out, b| {
        write!(out, "{b:02x}").unwrap();
        out
    })
}

fn id(text: &str) -> StoredId {
    StoredId::new(text).unwrap()
}

fn long_value(len: usize) -> String {
    (0..len)
        .map(|i| char::from(b'a' + u8::try_from((i * 7 + i / 26) % 26).unwrap()))
        .collect()
}

fn os_store(service: &str) -> IndexedStore<ChunkedStore<KeyringStore>> {
    IndexedStore::new(ChunkedStore::new(KeyringStore::named(service)))
}

#[expect(
    clippy::print_stderr,
    reason = "a skip a reader of the test output can see and read the reason for"
)]
fn keyring_works(service: &str) -> bool {
    let raw = KeyringStore::named(service);
    let probe = id("probe");
    let works = keyring_available()
        && raw.set(&probe, &Secret::new("v".to_owned())).is_ok()
        && raw.delete(&probe).is_ok();
    if !works {
        assert!(
            std::env::var_os("PUDDLE_REQUIRE_OS_CREDENTIAL_STORE").is_none(),
            "PUDDLE_REQUIRE_OS_CREDENTIAL_STORE is set but this computer has no credential store that works"
        );
        eprintln!("skipped: this computer has no credential store that works");
    }
    works
}

/// What the test wrote under its own service name, long values' pieces included, removed when the
/// test ends or panics: a failed run must not leave entries in the user's credential store.
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
fn values_move_to_the_file_vault_and_back_over_the_real_keyring() {
    let service = format!("puddle-migrate-{}", random_hex());
    if !keyring_works(&service) {
        return;
    }
    let names = ["case-long", "case-unicode", "case-plain"];
    let ids: Vec<StoredId> = names.iter().map(|n| id(n)).collect();
    // Long enough for nine entries of the Windows store, and no longer: Credential Manager holds
    // about 450 KB for all of a user's programs together and refuses writes past that.
    let values = [
        long_value(8192),
        "pa\u{df}w\u{f6}rd \u{1f986} \u{65e5}\u{672c}\u{8a9e}".to_owned(),
        "plain".to_owned(),
    ];
    let _cleanup = Cleanup {
        store: ChunkedStore::new(KeyringStore::named(&service)),
        ids: ids.iter().cloned().chain([id("puddle-index")]).collect(),
    };
    let dir = tempfile::tempdir().unwrap();
    let os = os_store(&service);
    let vault = IndexedStore::new(
        FileVault::create(
            &dir.path().join("vault"),
            VaultKey::from_bytes([9; 32]),
            KeySourceTag::KeyFile,
        )
        .unwrap(),
    );
    for (id, value) in ids.iter().zip(&values) {
        os.set(id, &Secret::new(value.clone())).unwrap();
    }

    let moved = move_values(&os, &vault, &ids, &mut || Ok(())).unwrap();
    assert_eq!((moved.copied, moved.left), (3, 0));
    for (id, value) in ids.iter().zip(&values) {
        assert_eq!(vault.get(id).unwrap().unwrap().expose(), value);
        assert!(os.get(id).unwrap().is_none(), "the keyring gave it up");
    }

    // A crash after the switch leaves values behind; the rerun removes them and keeps the
    // store in charge's value.
    os.set(&ids[2], &Secret::new("left behind".to_owned()))
        .unwrap();
    let cleared = clear_old_store(&os, &ids);
    assert_eq!((cleared.removed, cleared.left), (1, 0));
    assert!(os.get(&ids[2]).unwrap().is_none());
    assert_eq!(vault.get(&ids[2]).unwrap().unwrap().expose(), "plain");

    // And back again.
    let back = move_values(&vault, &os, &ids, &mut || Ok(())).unwrap();
    assert_eq!((back.copied, back.left), (3, 0));
    for (id, value) in ids.iter().zip(&values) {
        assert_eq!(os.get(id).unwrap().unwrap().expose(), value);
        assert!(vault.get(id).unwrap().is_none());
    }
}
