// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

fn id() -> StoredId {
    StoredId::new("probe").unwrap()
}

#[test]
fn the_store_is_filed_under_puddle_unless_a_test_names_another_service() {
    assert_eq!(KeyringStore::new().service, None);
    assert_eq!(
        KeyringStore::named("puddle-test-x").service.as_deref(),
        Some("puddle-test-x")
    );
}

#[test]
fn only_an_entry_reported_as_local_is_kept_local() {
    let with = |value: &str| {
        std::collections::HashMap::from([("persistence".to_owned(), value.to_owned())])
    };
    assert!(is_kept_local(&with("Local")));
    assert!(!is_kept_local(&with("Enterprise")));
    assert!(!is_kept_local(&std::collections::HashMap::new()));
}

#[test]
fn availability_matches_whether_a_write_to_the_platform_store_works() {
    let store = KeyringStore::named("puddle-test-availability");
    let works = store.set(&id(), &Secret::new("v".to_owned())).is_ok();
    if works {
        store.delete(&id()).unwrap();
    }
    assert_eq!(keyring_available(), works);
}

#[test]
fn a_computer_without_a_credential_store_fails_every_call_and_loses_nothing_silently() {
    if !keyring_available() {
        let store = KeyringStore::named("puddle-test-unavailable");
        assert_eq!(store.get(&id()).err(), Some(StoreError));
        assert_eq!(
            store.set(&id(), &Secret::new("v".to_owned())),
            Err(StoreError)
        );
        assert_eq!(store.delete(&id()), Err(StoreError));
    }
}
