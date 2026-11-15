// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;
use crate::store::MemoryStore;

#[test]
fn a_store_that_takes_the_write_is_working_and_keeps_nothing() {
    let store = MemoryStore::new();
    assert_eq!(
        probe_store(&store, || panic!("only a failed write asks")),
        StoreHealth::Working
    );
    assert_eq!(store.ids(), Vec::<String>::new());
}

#[test]
fn a_store_that_refuses_the_write_while_its_service_answers_is_locked() {
    let store = MemoryStore::new();
    store.break_it();
    assert_eq!(probe_store(&store, || true), StoreHealth::Locked);
}

#[test]
fn a_store_that_refuses_the_write_with_no_service_is_absent() {
    let store = MemoryStore::new();
    store.break_it();
    assert_eq!(probe_store(&store, || false), StoreHealth::NoProvider);
}

#[test]
fn a_store_that_works_again_is_working_again() {
    let store = MemoryStore::new();
    store.break_it();
    store.heal();
    assert_eq!(probe_store(&store, || true), StoreHealth::Working);
}

#[test]
fn a_store_that_writes_but_cannot_delete_still_counts_as_working() {
    struct NoDelete;
    impl SecretStore for NoDelete {
        fn get(&self, _: &StoredId) -> Result<Option<Secret>, crate::StoreError> {
            Ok(None)
        }
        fn set(&self, _: &StoredId, _: &Secret) -> Result<(), crate::StoreError> {
            Ok(())
        }
        fn delete(&self, _: &StoredId) -> Result<(), crate::StoreError> {
            Err(crate::StoreError)
        }
    }
    assert_eq!(probe_store(&NoDelete, || true), StoreHealth::Working);
}

#[test]
fn a_working_store_has_no_advice() {
    assert_eq!(StoreHealth::Working.advice(true), None);
    assert_eq!(StoreHealth::Working.advice(false), None);
}

#[test]
fn the_linux_advice_names_the_cause_the_fix_and_the_restart() {
    let absent = StoreHealth::NoProvider.advice(true).unwrap();
    assert!(absent.title.contains("no credential store is running"));
    assert!(absent.fix.contains("apt install gnome-keyring"));
    assert!(absent.fix.contains("restart Puddle"));
    let locked = StoreHealth::Locked.advice(true).unwrap();
    assert!(locked.title.contains("locked"));
    assert!(
        locked
            .fix
            .contains("gnome-keyring-daemon --daemonize --login")
    );
    assert!(locked.fix.contains("restart Puddle"));
}

#[test]
fn the_advice_for_other_platforms_does_not_send_the_user_to_gnome_keyring() {
    for health in [StoreHealth::NoProvider, StoreHealth::Locked] {
        let advice = health.advice(false).unwrap();
        assert!(!advice.fix.contains("gnome-keyring"));
        assert!(advice.fix.contains("restart Puddle"));
    }
}

#[test]
fn the_real_probe_agrees_with_a_direct_write_to_the_platform_store() {
    let direct = KeyringStore::named("puddle-test-probe-agreement");
    let id = StoredId::new("agreement").unwrap();
    let works = direct.set(&id, &Secret::new("v".to_owned())).is_ok();
    if works {
        direct.delete(&id).unwrap();
    }
    assert_eq!(probe_os_store() == StoreHealth::Working, works);
}

#[test]
fn an_entry_name_the_store_refuses_is_reported_as_a_refusing_store() {
    let store = MemoryStore::new();
    assert_eq!(
        probe_entry(&store, "not a valid/id".to_owned(), || panic!(
            "no write was tried"
        )),
        StoreHealth::Locked
    );
    assert_eq!(store.ids(), Vec::<String>::new());
}
