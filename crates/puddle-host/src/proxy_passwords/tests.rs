// SPDX-License-Identifier: AGPL-3.0-or-later
use puddle_secrets::{MemoryStore, Secret, SecretStore, StoreError, StoredId};
use puddle_types::fixture;

use super::*;

/// The password the tests plant and look for; random per run.
fn secret() -> String {
    fixture::canary("proxy-password")
}

fn proxy() -> ProxyAddr {
    ProxyAddr::new("Proxy.Corp", 8080)
}

fn vault() -> Arc<MemoryStore> {
    Arc::new(MemoryStore::new())
}

#[test]
fn a_sign_in_survives_a_restart_and_answers_from_memory() {
    let vault = vault();
    let first = ProxyPasswords::new(vault.clone());
    first.set(&proxy(), "corp\\alice", &secret()).unwrap();
    assert_eq!(first.credentials(&proxy()).unwrap().user(), "corp\\alice");

    let second = ProxyPasswords::new(vault);
    assert!(
        second.credentials(&proxy()).is_none(),
        "nothing is known before load"
    );
    assert_eq!(second.load().unwrap(), 1);
    assert_eq!(second.proxies(), [(proxy(), "corp\\alice".to_owned())]);
    assert!(
        second
            .credentials(&ProxyAddr::new("other.corp", 8080))
            .is_none()
    );
}

#[test]
fn nothing_but_the_credential_store_holds_the_password() {
    let vault = vault();
    let passwords = ProxyPasswords::new(vault.clone());
    passwords.set(&proxy(), "alice", &secret()).unwrap();
    for text in [
        format!("{passwords:?}"),
        format!("{:?}", passwords.proxies()),
    ] {
        fixture::assert_absent("a debug rendering", &text, &secret());
    }
    // Two entries, both under the prefix the clean-up keeps; the id does not spell the host.
    let ids = vault.ids();
    assert_eq!(ids.len(), 2, "the store holds two entries");
    assert!(
        ids.iter().all(|id| id.starts_with(ID_PREFIX)),
        "an entry id lacks the prefix the clean-up keeps"
    );
    assert!(
        ids.iter().all(|id| !id.contains("corp")),
        "an entry id spells the host"
    );
}

#[test]
fn removing_a_sign_in_leaves_nothing_in_the_store() {
    let vault = vault();
    let passwords = ProxyPasswords::new(vault.clone());
    passwords.set(&proxy(), "alice", &secret()).unwrap();
    passwords.remove(&proxy()).unwrap();
    passwords.remove(&proxy()).unwrap();
    assert!(passwords.credentials(&proxy()).is_none());
    assert_eq!(passwords.proxies(), []);
    assert_eq!(vault.ids(), Vec::<String>::new());
}

#[test]
fn setting_again_replaces_and_each_proxy_keeps_its_own() {
    let passwords = ProxyPasswords::new(vault());
    let other = ProxyAddr::new("other.corp", 3128);
    passwords
        .set(&proxy(), "alice", &fixture::password("one"))
        .unwrap();
    passwords
        .set(&other, "bob", &fixture::password("two"))
        .unwrap();
    passwords
        .set(&proxy(), "alice", &fixture::password("three"))
        .unwrap();
    assert_eq!(
        passwords.proxies(),
        [(other, "bob".to_owned()), (proxy(), "alice".to_owned())]
    );
    let again = ProxyPasswords::new(passwords.vault.clone());
    assert_eq!(again.load().unwrap(), 2);
}

#[test]
fn a_user_or_password_basic_cannot_carry_is_refused_and_nothing_is_stored() {
    let vault = vault();
    let passwords = ProxyPasswords::new(vault.clone());
    for user in ["", "a:b", "a\nb", "a\rb", &"u".repeat(257)] {
        assert_eq!(
            passwords.set(&proxy(), user, &fixture::password("p")),
            Err(ProxyPasswordError::BadUser),
            "{user:?}"
        );
    }
    let (line, carriage) = (
        format!("{}\n{}", fixture::password("a"), fixture::password("b")),
        format!("{}\r{}", fixture::password("a"), fixture::password("b")),
    );
    for bad in [String::new(), line, carriage] {
        assert_eq!(
            passwords.set(&proxy(), "alice", &bad),
            Err(ProxyPasswordError::BadPassword),
            "a password Basic cannot carry was accepted"
        );
    }
    assert_eq!(vault.ids(), Vec::<String>::new());
    // A password may hold a colon: Basic splits at the first one only.
    passwords
        .set(
            &proxy(),
            "alice",
            &format!("{}:x", fixture::password("colon")),
        )
        .unwrap();
}

#[test]
fn a_store_that_fails_changes_nothing_in_memory() {
    let vault = vault();
    let passwords = ProxyPasswords::new(vault.clone());
    passwords
        .set(&proxy(), "alice", &fixture::password("one"))
        .unwrap();
    vault.break_it();
    assert_eq!(
        passwords.set(&proxy(), "alice", &fixture::password("two")),
        Err(ProxyPasswordError::Store)
    );
    assert_eq!(passwords.remove(&proxy()), Err(ProxyPasswordError::Store));
    assert_eq!(passwords.load(), Err(ProxyPasswordError::Store));
    // The old sign-in still answers, and still after the store is back.
    assert!(passwords.credentials(&proxy()).is_some());
    vault.heal();
    assert_eq!(passwords.load().unwrap(), 1);
}

#[test]
fn a_change_after_a_load_that_failed_keeps_every_other_sign_in() {
    let vault = vault();
    let earlier = ProxyPasswords::new(vault.clone());
    let (a, b, c) = (
        ProxyAddr::new("a.corp", 8080),
        ProxyAddr::new("b.corp", 8080),
        ProxyAddr::new("c.corp", 8080),
    );
    earlier.set(&a, "alice", &fixture::password("a")).unwrap();
    earlier.set(&c, "carol", &fixture::password("c")).unwrap();
    // This run starts while the store is locked, so it knows no sign-in.
    let passwords = ProxyPasswords::new(vault.clone());
    vault.break_it();
    assert_eq!(passwords.load(), Err(ProxyPasswordError::Store));
    // The user unlocks it, saves one and takes another out.
    vault.heal();
    passwords.set(&b, "bob", &fixture::password("b")).unwrap();
    passwords.remove(&c).unwrap();
    assert_eq!(
        passwords.proxies(),
        [
            (a.clone(), "alice".to_owned()),
            (b.clone(), "bob".to_owned())
        ]
    );
    let later = ProxyPasswords::new(vault);
    assert_eq!(later.load().unwrap(), 2);
    assert_eq!(
        later.proxies(),
        [(a, "alice".to_owned()), (b, "bob".to_owned())]
    );
}

/// A listed sign-in this run could not read stays listed through a change of another, so a later
/// run that can read it gets it back.
#[test]
fn a_change_keeps_a_listed_sign_in_this_run_could_not_read() {
    let vault = vault();
    let passwords = ProxyPasswords::new(vault.clone());
    let other = ProxyAddr::new("other.corp", 3128);
    passwords
        .set(&proxy(), "alice", &fixture::password("one"))
        .unwrap();
    let entry = passwords.entry_id(&proxy()).unwrap();
    let saved = vault.get(&entry).unwrap().unwrap();
    vault
        .set(&entry, &Secret::new("no newline".into()))
        .unwrap();
    let again = ProxyPasswords::new(vault.clone());
    assert_eq!(again.load().unwrap(), 0);
    again.set(&other, "bob", &fixture::password("two")).unwrap();
    vault.set(&entry, &saved).unwrap();
    let later = ProxyPasswords::new(vault);
    assert_eq!(later.load().unwrap(), 2);
}

#[test]
fn a_listed_proxy_without_a_readable_entry_is_skipped_on_load() {
    let vault = vault();
    let passwords = ProxyPasswords::new(vault.clone());
    let other = ProxyAddr::new("other.corp", 3128);
    passwords
        .set(&proxy(), "alice", &fixture::password("one"))
        .unwrap();
    passwords
        .set(&other, "bob", &fixture::password("two"))
        .unwrap();
    // A crash between the list and the entry, and an entry that is not a sign-in.
    vault
        .delete(&passwords.entry_id(&proxy()).unwrap())
        .unwrap();
    vault
        .set(
            &passwords.entry_id(&other).unwrap(),
            &Secret::new("no newline".into()),
        )
        .unwrap();
    let again = ProxyPasswords::new(vault);
    assert_eq!(again.load().unwrap(), 0);
    assert_eq!(again.proxies(), []);
}

#[test]
fn a_list_line_that_is_not_a_proxy_is_ignored() {
    let vault = vault();
    let passwords = ProxyPasswords::new(vault.clone());
    vault
        .set(
            &passwords.list_id().unwrap(),
            &Secret::new("\n://\nproxy.corp:8080".into()),
        )
        .unwrap();
    assert_eq!(passwords.load().unwrap(), 0);
}

/// A store whose first `set` waits until the test lets it go, like a locked keyring.
struct SlowStore {
    inner: MemoryStore,
    entered: std::sync::mpsc::Sender<()>,
    release: Mutex<std::sync::mpsc::Receiver<()>>,
}

impl SecretStore for SlowStore {
    fn get(&self, id: &StoredId) -> Result<Option<Secret>, StoreError> {
        self.inner.get(id)
    }
    fn set(&self, id: &StoredId, secret: &Secret) -> Result<(), StoreError> {
        let _ = self.entered.send(());
        let _ = self.release.lock().unwrap().recv();
        self.inner.set(id, secret)
    }
    fn delete(&self, id: &StoredId) -> Result<(), StoreError> {
        self.inner.delete(id)
    }
}

#[test]
fn reading_the_sign_ins_never_waits_for_a_slow_credential_store() {
    let (entered_tx, entered) = std::sync::mpsc::channel();
    let (release_tx, release) = std::sync::mpsc::channel();
    let slow = Arc::new(SlowStore {
        inner: MemoryStore::new(),
        entered: entered_tx,
        release: Mutex::new(release),
    });
    let passwords = ProxyPasswords::new(slow);
    let writer = {
        let passwords = passwords.clone();
        std::thread::spawn(move || passwords.set(&proxy(), "alice", &secret()))
    };
    entered
        .recv_timeout(std::time::Duration::from_secs(20))
        .expect("the set reached the store");

    // The network-health page and the proxy's lookups run now; they get an answer at once.
    let (done_tx, done) = std::sync::mpsc::channel();
    let reader = {
        let passwords = passwords.clone();
        std::thread::spawn(move || {
            let listed = passwords.proxies();
            let found = passwords.credentials(&proxy());
            let _ = done_tx.send((listed, found.is_some()));
        })
    };
    let answered = done.recv_timeout(std::time::Duration::from_secs(5));
    // Let the store go before asserting, so a failure does not leave the threads hanging.
    for _ in 0..4 {
        let _ = release_tx.send(());
    }
    writer.join().unwrap().unwrap();
    reader.join().unwrap();
    assert_eq!(
        answered.expect("a read waited for the credential store"),
        (Vec::new(), false)
    );
    assert_eq!(passwords.proxies(), [(proxy(), "alice".to_owned())]);
}

#[test]
fn a_vault_that_comes_back_empty_leaves_the_signed_in_proxies_named_as_lost_until_entered_again() {
    let dir = tempfile::tempdir().unwrap();
    let known = dir.path().join("proxy-sign-ins");
    let first = ProxyPasswords::with_known_file(vault(), known.clone());
    first.set(&proxy(), "alice", &secret()).unwrap();
    let other = ProxyAddr::new("other.corp", 3128);
    first.set(&other, "bob", &fixture::password("two")).unwrap();

    // A reset: a new, empty vault; the known file stays.
    let reset = ProxyPasswords::with_known_file(vault(), known.clone());
    assert_eq!(reset.load().unwrap(), 0);
    assert_eq!(reset.lost(), [proxy(), other.clone()]);
    assert_eq!(reset.proxies(), []);

    // Entering one again takes it off; removing the other takes it off too.
    reset.set(&proxy(), "alice", &secret()).unwrap();
    reset.remove(&other).unwrap();
    assert_eq!(reset.lost(), []);
    let again = ProxyPasswords::with_known_file(reset.vault.clone(), known);
    assert_eq!(again.load().unwrap(), 1);
    assert_eq!(again.lost(), []);
}
