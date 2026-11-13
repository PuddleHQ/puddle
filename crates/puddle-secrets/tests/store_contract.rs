// SPDX-License-Identifier: GPL-3.0-or-later
//! One set of cases every secret store must pass, run on the store in memory, on the file vault, and
//! on the operating system's credential store where the computer has one that works (Credential
//! Manager on Windows, a Secret Service on Linux). A computer without one skips that run and says
//! so, unless `PUDDLE_REQUIRE_OS_CREDENTIAL_STORE` is set: the Windows job and the Linux job that
//! starts gnome-keyring set it, so a runner whose store stopped working fails instead.
//!
//! Each backend gives a way to open another handle over the same storage (what a second process
//! sees), and, where the storage can be made to fail, a way to break and mend it.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code: a panic is how a test fails"
)]

use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Arc;

use puddle_secrets::{
    ChunkedStore, FileVault, IndexedStore, KeySourceTag, KeyringStore, MemoryStore, Secret,
    SecretStore, StoreError, StoredId, Sweep, VaultKey, keyring_available,
};

/// A handle over a store somebody else also holds.
struct Shared<S>(Arc<S>);

impl<S: SecretStore> SecretStore for Shared<S> {
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

/// One case of the suite.
type Case = fn(&Backend);

struct Backend {
    name: &'static str,
    /// The longest value the case "a large value round-trips" writes. 64 KiB, except over the
    /// operating system store: `ChunkedStore` keeps at most 64 pieces of 1000 UTF-16 units, so it
    /// refuses a value past 64000 characters (a token is never near that), and the case there
    /// writes just under it.
    large: usize,
    /// A new handle over the same storage, wrapped the way the product wraps it.
    open: Box<dyn Fn() -> Box<dyn SecretStore>>,
    /// Makes every call fail until [`Self::mend`]; `None` where the storage cannot be broken.
    break_it: Option<Box<dyn Fn()>>,
    mend: Option<Box<dyn Fn()>>,
    /// Ids the run may have left in real storage, removed when the backend drops.
    cleanup: Vec<Box<dyn Fn()>>,
    _keep: Vec<Box<dyn std::any::Any>>,
}

impl Drop for Backend {
    fn drop(&mut self) {
        for clean in &self.cleanup {
            clean();
        }
    }
}

fn memory() -> Backend {
    let store = Arc::new(MemoryStore::new());
    let (open_from, broken, healed) = (store.clone(), store.clone(), store);
    Backend {
        name: "memory",
        large: 64 * 1024,
        open: Box::new(move || Box::new(IndexedStore::new(Shared(open_from.clone())))),
        break_it: Some(Box::new(move || broken.break_it())),
        mend: Some(Box::new(move || healed.heal())),
        cleanup: Vec::new(),
        _keep: Vec::new(),
    }
}

fn vault_key() -> VaultKey {
    VaultKey::from_bytes([42; 32])
}

fn file_vault() -> Backend {
    let dir = tempfile::tempdir().unwrap();
    let path: PathBuf = dir.path().join("secrets").join("vault");
    FileVault::create(&path, vault_key(), KeySourceTag::KeyFile).unwrap();
    let (open_path, break_path) = (path.clone(), path.clone());
    let mend_path = path;
    let aside = dir.path().join("aside");
    let (aside_break, aside_mend) = (aside.clone(), aside);
    Backend {
        name: "file vault",
        large: 64 * 1024,
        open: Box::new(move || {
            Box::new(IndexedStore::new(
                FileVault::open(&open_path, vault_key()).unwrap(),
            ))
        }),
        // A vault that cannot be read: the file is moved away, and put back to mend it.
        break_it: Some(Box::new(move || {
            std::fs::rename(&break_path, &aside_break).unwrap();
        })),
        mend: Some(Box::new(move || {
            std::fs::rename(&aside_mend, &mend_path).unwrap();
        })),
        cleanup: Vec::new(),
        _keep: vec![Box::new(dir)],
    }
}

fn random_hex() -> String {
    let mut bytes = [0_u8; 8];
    getrandom::fill(&mut bytes).unwrap();
    bytes.iter().fold(String::new(), |mut out, b| {
        write!(out, "{b:02x}").unwrap();
        out
    })
}

/// The platform store, or `None` (with a printed reason) where the computer has none that works.
#[expect(
    clippy::print_stderr,
    reason = "a skip a reader of the test output can see and read the reason for"
)]
fn os_store() -> Option<Backend> {
    let service = format!("puddle-contract-{}", random_hex());
    let raw = KeyringStore::named(&service);
    let probe = StoredId::new("probe").unwrap();
    // `keyring_available` is true with a locked keyring; a write says whether it really works.
    let works = keyring_available()
        && raw.set(&probe, &Secret::new("v".to_owned())).is_ok()
        && raw.delete(&probe).is_ok();
    if !works {
        assert!(
            std::env::var_os("PUDDLE_REQUIRE_OS_CREDENTIAL_STORE").is_none(),
            "PUDDLE_REQUIRE_OS_CREDENTIAL_STORE is set but this computer has no credential store that works"
        );
        eprintln!("skipped: this computer has no credential store that works");
        return None;
    }
    let sweep = KeyringStore::named(&service);
    Some(Backend {
        name: "operating system store",
        large: 63_000,
        open: Box::new(move || {
            Box::new(IndexedStore::new(ChunkedStore::new(KeyringStore::named(
                &service,
            ))))
        }),
        break_it: None,
        mend: None,
        // Whatever the cases wrote: every id they use starts with `case-`; the index is a fixed id.
        cleanup: vec![Box::new(move || {
            let store = ChunkedStore::new(sweep.clone());
            let _ = store.delete(&StoredId::new("puddle-index").unwrap());
            for case in CASES {
                for n in 0..8 {
                    let _ = store.delete(&case_id(case, n));
                }
            }
        })],
        _keep: Vec::new(),
    })
}

fn case_id(case: &str, n: usize) -> StoredId {
    StoredId::new(format!("case-{case}-{n}")).unwrap()
}

fn secret(text: &str) -> Secret {
    Secret::new(text.to_owned())
}

fn value_of(store: &dyn SecretStore, id: &StoredId) -> Option<String> {
    store.get(id).unwrap().map(|s| s.expose().to_owned())
}

/// A value of `len` characters that differs from place to place, so a piece in the wrong order
/// reads back as another value.
fn long_value(len: usize) -> String {
    (0..len)
        .map(|i| char::from(b'a' + u8::try_from((i * 7 + i / 26) % 26).unwrap()))
        .collect()
}

const CASES: [&str; 8] = [
    "missing", "replace", "delete", "large", "unicode", "sweep", "broken", "second",
];

fn a_missing_id_is_none_not_an_error(b: &Backend) {
    let store = (b.open)();
    assert!(store.get(&case_id("missing", 0)).unwrap().is_none());
}

fn a_value_reads_back_and_a_second_set_replaces_it(b: &Backend) {
    let store = (b.open)();
    let id = case_id("replace", 0);
    store.set(&id, &secret("first")).unwrap();
    assert_eq!(value_of(&*store, &id).as_deref(), Some("first"));
    store.set(&id, &secret("second, and longer")).unwrap();
    assert_eq!(
        value_of(&*store, &id).as_deref(),
        Some("second, and longer")
    );
    store.set(&id, &secret("3")).unwrap();
    assert_eq!(value_of(&*store, &id).as_deref(), Some("3"));
    // Another handle over the same storage sees it: nothing lives only in the first one.
    assert_eq!(value_of(&*(b.open)(), &id).as_deref(), Some("3"));
}

fn deleting_removes_the_value_and_a_missing_id_is_fine(b: &Backend) {
    let store = (b.open)();
    let (kept, gone) = (case_id("delete", 0), case_id("delete", 1));
    store.set(&kept, &secret("stays")).unwrap();
    store.set(&gone, &secret("goes")).unwrap();
    store.delete(&gone).unwrap();
    store.delete(&gone).unwrap();
    store.delete(&case_id("delete", 2)).unwrap();
    assert!(store.get(&gone).unwrap().is_none());
    assert_eq!(value_of(&*store, &kept).as_deref(), Some("stays"));
}

fn a_large_value_round_trips(b: &Backend) {
    let store = (b.open)();
    let id = case_id("large", 0);
    let value = long_value(b.large);
    store.set(&id, &secret(&value)).unwrap();
    assert_eq!(value_of(&*store, &id).as_deref(), Some(value.as_str()));
    store.set(&id, &secret("short")).unwrap();
    assert_eq!(value_of(&*store, &id).as_deref(), Some("short"));
}

fn unicode_and_awkward_values_round_trip(b: &Backend) {
    let store = (b.open)();
    for (n, text) in [
        "pässwörd-日本語-🦆",
        "line one\nline two\r\n\ttabbed",
        "  leading and trailing  ",
        "nul\u{0}inside",
        "\"quotes\" and \\ backslashes and 'single'",
    ]
    .into_iter()
    .enumerate()
    {
        let id = case_id("unicode", n);
        store.set(&id, &secret(text)).unwrap();
        assert_eq!(value_of(&*store, &id).as_deref(), Some(text), "case {n}");
    }
}

fn the_index_lists_what_was_written_and_the_sweep_removes_what_nothing_names(b: &Backend) {
    let (kept, orphan, long) = (
        case_id("sweep", 0),
        case_id("sweep", 1),
        case_id("sweep", 2),
    );
    let earlier = (b.open)();
    earlier.set(&kept, &secret("kept")).unwrap();
    earlier.set(&orphan, &secret("orphan")).unwrap();
    earlier.set(&long, &secret(&long_value(5000))).unwrap();

    // A new handle (a new process) finds what the earlier one listed.
    let later = (b.open)();
    let swept = later.sweep_orphans(&|id| *id == kept).unwrap();
    assert_eq!(
        swept,
        Sweep {
            removed: 2,
            stuck: 0
        }
    );
    assert_eq!(value_of(&*later, &kept).as_deref(), Some("kept"));
    assert!(later.get(&orphan).unwrap().is_none());
    assert!(later.get(&long).unwrap().is_none());
    assert_eq!(
        later.sweep_orphans(&|_| false).unwrap(),
        Sweep {
            removed: 1,
            stuck: 0
        }
    );
    assert!(later.get(&kept).unwrap().is_none());
    assert_eq!(later.sweep_orphans(&|_| false).unwrap(), Sweep::default());

    // A delete takes the id out of the index too.
    let again = (b.open)();
    again.set(&kept, &secret("again")).unwrap();
    again.delete(&kept).unwrap();
    assert_eq!(
        (b.open)().sweep_orphans(&|_| false).unwrap(),
        Sweep::default()
    );
    // The index is not a credential.
    assert!(
        again
            .set(&StoredId::new("puddle-index").unwrap(), &secret("x"))
            .is_err()
    );
}

fn a_broken_store_answers_store_error_and_recovers_with_its_values(b: &Backend) {
    let (Some(break_it), Some(mend)) = (&b.break_it, &b.mend) else {
        return;
    };
    let store = (b.open)();
    let id = case_id("broken", 0);
    store.set(&id, &secret("before")).unwrap();
    break_it();
    assert_eq!(store.get(&id).err(), Some(StoreError));
    assert_eq!(store.set(&id, &secret("during")), Err(StoreError));
    assert_eq!(store.delete(&id), Err(StoreError));
    assert_eq!(store.sweep_orphans(&|_| true).err(), Some(StoreError));
    mend();
    // The refused write changed nothing.
    assert_eq!(value_of(&*store, &id).as_deref(), Some("before"));
}

fn many_ids_in_one_store_do_not_disturb_each_other(b: &Backend) {
    let store = (b.open)();
    for n in 0..8 {
        store
            .set(&case_id("second", n), &secret(&format!("value {n}")))
            .unwrap();
    }
    store.delete(&case_id("second", 3)).unwrap();
    store
        .set(&case_id("second", 5), &secret("five again"))
        .unwrap();
    for n in 0..8 {
        let expected = match n {
            3 => None,
            5 => Some("five again".to_owned()),
            _ => Some(format!("value {n}")),
        };
        assert_eq!(value_of(&*store, &case_id("second", n)), expected, "id {n}");
    }
}

/// Every case on a backend of its own, so what one case leaves behind (the sweep removes what
/// nothing names) cannot reach another.
fn run_suite(make: &dyn Fn() -> Option<Backend>) {
    let cases: [(&str, Case); 8] = [
        ("missing", a_missing_id_is_none_not_an_error),
        ("replace", a_value_reads_back_and_a_second_set_replaces_it),
        (
            "delete",
            deleting_removes_the_value_and_a_missing_id_is_fine,
        ),
        ("large", a_large_value_round_trips),
        ("unicode", unicode_and_awkward_values_round_trip),
        (
            "sweep",
            the_index_lists_what_was_written_and_the_sweep_removes_what_nothing_names,
        ),
        (
            "broken",
            a_broken_store_answers_store_error_and_recovers_with_its_values,
        ),
        ("second", many_ids_in_one_store_do_not_disturb_each_other),
    ];
    for (name, case) in cases {
        let Some(backend) = make() else { return };
        eprintln_name(&format!("{} / {name}", backend.name));
        case(&backend);
    }
}

#[expect(
    clippy::print_stderr,
    reason = "names the backend in the test output, so a failure says which one"
)]
fn eprintln_name(name: &str) {
    eprintln!("store contract: {name}");
}

#[test]
fn the_store_in_memory_keeps_the_contract() {
    run_suite(&|| Some(memory()));
}

#[test]
fn the_file_vault_keeps_the_contract() {
    run_suite(&|| Some(file_vault()));
}

#[test]
fn the_operating_system_store_keeps_the_contract() {
    run_suite(&os_store);
}

#[test]
fn a_bare_memory_store_has_no_index_and_sweeps_nothing() {
    let store = MemoryStore::new();
    store.set(&case_id("sweep", 0), &secret("v")).unwrap();
    assert_eq!(store.sweep_orphans(&|_| false).unwrap(), Sweep::default());
}
