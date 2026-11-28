// SPDX-License-Identifier: AGPL-3.0-or-later
use std::sync::Arc;

use super::*;
use crate::clock::ManualClock;
use crate::store::Limits;

fn setup() -> (Store, Arc<ManualClock>) {
    let clock = Arc::new(ManualClock::new(1_000));
    let store = Store::open_in_memory(clock.clone(), Limits::default()).unwrap();
    (store, clock)
}

fn ann(store: &Store) -> UserId {
    store.add_user(&UserName::new("ann").unwrap()).unwrap().id
}

#[test]
fn a_user_is_found_by_name_ignoring_case_and_a_bad_name_finds_nobody() {
    let (store, _) = setup();
    assert_eq!(
        store.user_named("OWNER").unwrap().unwrap().id,
        UserId::OWNER
    );
    assert_eq!(store.user_named("nobody").unwrap(), None);
    assert_eq!(store.user_named("").unwrap(), None);
    assert_eq!(store.user_named("a\u{0}b").unwrap(), None);
}

#[test]
fn a_device_lists_for_its_user_only_until_revoked_or_expired() {
    let (store, clock) = setup();
    let ann = ann(&store);
    let mine = store.add_device(UserId::OWNER, " desktop ", 5_000).unwrap();
    assert_eq!(mine.name, "desktop");
    let hers = store.add_device(ann, "phone", 5_000).unwrap();
    assert_eq!(store.devices(UserId::OWNER).unwrap(), vec![mine.clone()]);
    assert_eq!(store.devices(ann).unwrap(), vec![hers.clone()]);

    // Revoking someone else's device does nothing.
    assert!(!store.revoke_device(UserId::OWNER, hers.id).unwrap());
    assert!(store.device_session(hers.id).unwrap().is_some());
    assert!(store.revoke_device(UserId::OWNER, mine.id).unwrap());
    assert!(
        !store.revoke_device(UserId::OWNER, mine.id).unwrap(),
        "twice is a no-op"
    );
    assert_eq!(store.devices(UserId::OWNER).unwrap(), Vec::new());
    assert_eq!(store.device_session(mine.id).unwrap(), None);

    clock.set(5_000);
    assert!(store.devices(ann).unwrap().is_empty(), "expired");
    assert_eq!(store.device_session(hers.id).unwrap(), None);
}

#[test]
fn a_used_device_stays_valid_longer_and_never_shorter() {
    let (store, clock) = setup();
    let device = store.add_device(UserId::OWNER, "desktop", 5_000).unwrap();
    clock.set(2_000);
    store.touch_device(device.id, 9_000).unwrap();
    let listed = store.devices(UserId::OWNER).unwrap();
    assert_eq!(listed[0].last_seen_at, 2_000);
    assert_eq!(listed[0].expires_at, 9_000);
    store.touch_device(device.id, 3_000).unwrap();
    assert_eq!(store.devices(UserId::OWNER).unwrap()[0].expires_at, 9_000);
}

#[test]
fn a_device_that_expired_or_whose_user_is_being_deleted_is_not_extended() {
    let (store, clock) = setup();
    let ann = ann(&store);
    let late = store.add_device(UserId::OWNER, "late", 5_000).unwrap();
    let hers = store.add_device(ann, "phone", 5_000).unwrap();
    // One millisecond before it lapses a use still extends it.
    clock.set(4_999);
    store.touch_device(late.id, 9_000).unwrap();
    assert_eq!(store.devices(UserId::OWNER).unwrap()[0].expires_at, 9_000);

    let lapsing = store.add_device(UserId::OWNER, "lapsing", 6_000).unwrap();
    clock.set(6_000);
    store.touch_device(lapsing.id, 20_000).unwrap();
    clock.set(5_000);
    let listed = store.devices(UserId::OWNER).unwrap();
    let lapsing = listed.iter().find(|d| d.id == lapsing.id).unwrap();
    assert_eq!((lapsing.expires_at, lapsing.last_seen_at), (6_000, 4_999));

    store.begin_user_delete(UserId::OWNER, ann).unwrap();
    clock.set(4_000);
    store.touch_device(hers.id, 20_000).unwrap();
    let kept = store
        .devices(ann)
        .unwrap()
        .into_iter()
        .find(|d| d.id == hers.id)
        .unwrap();
    assert_eq!((kept.expires_at, kept.last_seen_at), (5_000, 1_000));
}

#[test]
fn a_device_name_and_user_are_checked() {
    let (store, _) = setup();
    assert!(matches!(
        store.add_device(UserId::OWNER, "  ", 5_000),
        Err(StoreError::DeviceName(_))
    ));
    store
        .add_device(UserId::OWNER, &"x".repeat(64), 5_000)
        .unwrap();
    assert!(matches!(
        store.add_device(UserId::OWNER, &"x".repeat(65), 5_000),
        Err(StoreError::DeviceName(_))
    ));
    assert!(matches!(
        store.add_device(UserId::OWNER, "a\nb", 5_000),
        Err(StoreError::DeviceName(_))
    ));
    assert!(matches!(
        store.add_device(UserId::from_stored(77), "x", 5_000),
        Err(StoreError::UnknownUser(_))
    ));
}

#[test]
fn revoking_every_device_of_a_user_leaves_the_others_alone() {
    let (store, _) = setup();
    let ann = ann(&store);
    let a = store.add_device(UserId::OWNER, "a", 5_000).unwrap();
    let b = store.add_device(UserId::OWNER, "b", 5_000).unwrap();
    let c = store.add_device(ann, "c", 5_000).unwrap();
    let mut revoked = store.revoke_devices_of(UserId::OWNER, None).unwrap();
    revoked.sort();
    assert_eq!(revoked, vec![a.id, b.id]);
    assert_eq!(store.devices(ann).unwrap(), vec![c]);
    assert_eq!(
        store.revoke_devices_of(UserId::OWNER, None).unwrap(),
        Vec::new()
    );
}

#[test]
fn the_last_sign_in_method_that_is_on_stays_on() {
    let (store, _) = setup();
    assert_eq!(
        store.sign_in_methods().unwrap(),
        vec![(SignInMethod::Password, true)]
    );
    assert!(matches!(
        store.set_sign_in_method(SignInMethod::Password, false),
        Err(StoreError::SignInMethod(_))
    ));
    assert!(matches!(
        store.set_sign_in_method(SignInMethod::Passkey, true),
        Err(StoreError::SignInMethod(_))
    ));
    // Passkeys do not exist yet; this is what they will do once one is enrolled.
    store
        .register_sign_in_method(SignInMethod::Passkey)
        .unwrap();
    store
        .register_sign_in_method(SignInMethod::Passkey)
        .unwrap();
    store
        .set_sign_in_method(SignInMethod::Password, false)
        .unwrap();
    assert!(matches!(
        store.set_sign_in_method(SignInMethod::Passkey, false),
        Err(StoreError::SignInMethod(_))
    ));
    store
        .set_sign_in_method(SignInMethod::Password, true)
        .unwrap();
    store
        .set_sign_in_method(SignInMethod::Passkey, false)
        .unwrap();
    assert_eq!(
        store.sign_in_methods().unwrap(),
        vec![
            (SignInMethod::Passkey, false),
            (SignInMethod::Password, true)
        ]
    );
}

#[test]
fn a_password_change_keeps_the_device_that_made_it() {
    let (store, _) = setup();
    let keep = store.add_device(UserId::OWNER, "this one", 5_000).unwrap();
    let other = store.add_device(UserId::OWNER, "other", 5_000).unwrap();
    assert_eq!(
        store
            .revoke_devices_of(UserId::OWNER, Some(keep.id))
            .unwrap(),
        vec![other.id]
    );
    assert_eq!(store.devices(UserId::OWNER).unwrap(), vec![keep]);
}

#[test]
fn a_failing_database_is_an_error_not_an_empty_list() {
    let (store, _) = setup();
    let device = store.add_device(UserId::OWNER, "a", 5_000).unwrap();
    store.break_database("ALTER TABLE devices RENAME TO devices_gone");
    assert!(store.add_device(UserId::OWNER, "b", 5_000).is_err());
    assert!(store.devices(UserId::OWNER).is_err());
    assert!(store.device_session(device.id).is_err());
    assert!(store.touch_device(device.id, 9_000).is_err());
    assert!(store.revoke_device(UserId::OWNER, device.id).is_err());
    assert!(store.revoke_devices_of(UserId::OWNER, None).is_err());
    let (store, _) = setup();
    store.break_database("ALTER TABLE sign_in_methods RENAME TO methods_gone");
    assert!(store.sign_in_methods().is_err());
    assert!(
        store
            .register_sign_in_method(SignInMethod::Passkey)
            .is_err()
    );
    assert!(
        store
            .set_sign_in_method(SignInMethod::Password, true)
            .is_err()
    );
    let (store, _) = setup();
    store.break_database("ALTER TABLE users RENAME TO users_gone");
    assert!(store.user_named("owner").is_err());
}

#[test]
fn a_sweep_deletes_a_device_30_days_after_it_stopped_working_with_its_session() {
    const DAY: u64 = 24 * 60 * 60 * 1000;
    let (store, clock) = setup();
    let secrets = store.take_account_secrets().unwrap();
    let revoked = store
        .add_device(UserId::OWNER, "revoked", 10 * DAY)
        .unwrap();
    let lapsed = store.add_device(UserId::OWNER, "lapsed", 2_000).unwrap();
    let live = store.add_device(UserId::OWNER, "live", 90 * DAY).unwrap();
    for (digest, device) in [(1_u8, &revoked), (2, &lapsed), (3, &live)] {
        assert!(secrets.put_session(device.id, &[digest; 32]).unwrap());
    }
    store.revoke_device(UserId::OWNER, revoked.id).unwrap();
    let count = |sql: &str| -> i64 { lock(&store.conn).query_row(sql, [], |r| r.get(0)).unwrap() };
    // Revoked at 1_000, lapsed at 2_000: both stopped, neither for 30 days yet.
    clock.set(1_000 + 30 * DAY - 1);
    assert_eq!(store.sweep().unwrap().devices_removed, 0);
    assert_eq!(count("SELECT count(*) FROM devices"), 3);
    clock.set(1_000 + 30 * DAY);
    assert_eq!(store.sweep().unwrap().devices_removed, 1, "the revoked one");
    clock.set(2_000 + 30 * DAY);
    assert_eq!(store.sweep().unwrap().devices_removed, 1, "the lapsed one");
    assert_eq!(count("SELECT count(*) FROM devices"), 1);
    assert_eq!(count("SELECT count(*) FROM account_sessions"), 1);
    assert_eq!(store.sweep().unwrap().devices_removed, 0);
}
