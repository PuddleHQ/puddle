// SPDX-License-Identifier: GPL-3.0-or-later
//! The proxy sign-ins against the real operating system credential store, through entries no real
//! Puddle uses (the prefix has this process's id in it). It runs where a store exists (the Windows
//! job, a desktop session) and says so where it does not, unless `PUDDLE_REQUIRE_OS_CREDENTIAL_STORE`
//! is set (the Windows job sets it): then a missing store fails.

use std::sync::Arc;

use puddle_host::ProxyPasswords;
use puddle_secrets::{ChunkedStore, KeyringStore, SecretStore};
use puddle_upstream::{CredentialSource, ProxyAddr};

#[test]
fn the_os_store_keeps_a_test_owned_proxy_sign_in() {
    if !puddle_secrets::keyring_available() {
        assert!(
            std::env::var_os("PUDDLE_REQUIRE_OS_CREDENTIAL_STORE").is_none(),
            "PUDDLE_REQUIRE_OS_CREDENTIAL_STORE is set but this computer has no credential store that works"
        );
        eprintln!("no OS credential store here; skipped");
        return;
    }
    let prefix = format!("proxy-selftest-{}-", std::process::id());
    let os = || -> Arc<dyn SecretStore> { Arc::new(ChunkedStore::new(KeyringStore::new())) };
    let proxy = ProxyAddr::new("selftest.invalid", 3128);
    let first = ProxyPasswords::with_prefix(os(), &prefix);
    first
        .set(&proxy, "alice", &puddle_types::fixture::canary("os-store"))
        .unwrap();
    let second = ProxyPasswords::with_prefix(os(), &prefix);
    assert_eq!(second.load().unwrap(), 1);
    assert_eq!(second.credentials(&proxy).unwrap().user(), "alice");
    second.remove(&proxy).unwrap();
    let third = ProxyPasswords::with_prefix(os(), &prefix);
    assert_eq!(third.load().unwrap(), 0);
}
