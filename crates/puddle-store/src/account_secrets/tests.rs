// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use crate::clock::ManualClock;
use crate::store::{Limits, Store};
use crate::users::{Role, UserName};

fn setup() -> (Store, AccountSecrets, Arc<ManualClock>) {
    let clock = Arc::new(ManualClock::new(1_000));
    let store = Store::open_in_memory(clock.clone(), Limits::default()).unwrap();
    let secrets = store.take_account_secrets().unwrap();
    (store, secrets, clock)
}

#[test]
fn a_password_hash_reaches_only_the_check_it_is_given() {
    let (_store, secrets, _) = setup();
    let owner = UserId::OWNER;
    assert!(
        !secrets.check_password(owner, |_| true).unwrap(),
        "no password yet"
    );
    secrets.set_password_hash(owner, "$argon2id$one").unwrap();
    let mut seen = String::new();
    assert!(
        secrets
            .check_password(owner, |hash| {
                seen = hash.to_owned();
                true
            })
            .unwrap()
    );
    assert_eq!(seen, "$argon2id$one");
    assert!(!secrets.check_password(owner, |_| false).unwrap());
    secrets.set_password_hash(owner, "$argon2id$two").unwrap();
    assert!(
        secrets
            .check_password(owner, |h| h == "$argon2id$two")
            .unwrap()
    );
    secrets.clear_password(owner).unwrap();
    assert!(!secrets.check_password(owner, |_| true).unwrap());
}

#[test]
fn a_secret_for_a_user_that_does_not_exist_is_refused() {
    let (_store, secrets, _) = setup();
    let nobody = UserId::from_stored(42);
    assert!(matches!(
        secrets.set_password_hash(nobody, "h"),
        Err(StoreError::UnknownUser(_))
    ));
    assert!(matches!(
        secrets.put_invite(nobody, &[7; 32], 5_000),
        Err(StoreError::UnknownUser(_))
    ));
}

#[test]
fn an_invite_works_once_and_not_after_it_expires() {
    let (store, secrets, clock) = setup();
    let ann = store.add_user(&UserName::new("ann").unwrap()).unwrap().id;
    assert!(matches!(
        secrets.put_invite(ann, &[1; 8], 5_000),
        Err(StoreError::InviteInvalid)
    ));
    secrets.put_invite(ann, &[1; 32], 5_000).unwrap();
    // A new invite replaces the one before.
    secrets.put_invite(ann, &[2; 32], 5_000).unwrap();
    assert_eq!(secrets.redeem_invite(&[1; 32]).unwrap(), None);
    assert_eq!(secrets.redeem_invite(&[2; 32]).unwrap(), Some(ann));
    assert_eq!(secrets.redeem_invite(&[2; 32]).unwrap(), None, "used up");
    secrets.put_invite(ann, &[3; 32], 5_000).unwrap();
    clock.set(5_000);
    assert_eq!(secrets.redeem_invite(&[3; 32]).unwrap(), None, "expired");
}

#[test]
fn the_repository_never_shows_a_secret_in_its_debug_text() {
    let (_store, secrets, _) = setup();
    secrets
        .set_password_hash(UserId::OWNER, "$argon2id$x")
        .unwrap();
    assert_eq!(format!("{secrets:?}"), "AccountSecrets { .. }");
}

#[test]
fn an_invite_digest_of_exactly_the_minimum_length_is_accepted() {
    let (store, secrets, _) = setup();
    let ann = store.add_user(&UserName::new("ann").unwrap()).unwrap().id;
    let short = vec![4; MIN_INVITE_DIGEST_BYTES - 1];
    let exact = vec![5; MIN_INVITE_DIGEST_BYTES];
    assert!(matches!(
        secrets.put_invite(ann, &short, 5_000),
        Err(StoreError::InviteInvalid)
    ));
    secrets.put_invite(ann, &exact, 5_000).unwrap();
    assert_eq!(secrets.redeem_invite(&exact).unwrap(), Some(ann));
}

#[test]
fn the_first_visitor_makes_the_owner_once_and_a_reset_never_reopens_it() {
    let (store, secrets, _) = setup();
    assert!(!secrets.setup_done().unwrap());
    assert!(!secrets.has_password(UserId::OWNER).unwrap());
    secrets
        .set_password_hash(UserId::OWNER, "$argon2id$x")
        .unwrap();
    assert!(secrets.has_password(UserId::OWNER).unwrap());
    secrets.clear_password(UserId::OWNER).unwrap();
    let name = UserName::new("Tijs").unwrap();
    secrets.create_owner(&name, "$argon2id$first").unwrap();
    assert!(secrets.setup_done().unwrap());
    assert_eq!(store.user(UserId::OWNER).unwrap().name, "Tijs");
    assert!(
        secrets
            .check_password(UserId::OWNER, |h| h == "$argon2id$first")
            .unwrap()
    );
    // A second visitor is turned away and changes nothing.
    assert!(matches!(
        secrets.create_owner(&UserName::new("mallory").unwrap(), "$argon2id$evil"),
        Err(StoreError::SetupDone)
    ));
    assert_eq!(store.user(UserId::OWNER).unwrap().name, "Tijs");
    // Neither does a reset of the owner's password reopen it.
    secrets.clear_password(UserId::OWNER).unwrap();
    assert!(matches!(
        secrets.create_owner(&name, "$argon2id$again"),
        Err(StoreError::SetupDone)
    ));
    assert!(!secrets.has_password(UserId::OWNER).unwrap());
}

#[test]
fn the_owner_cannot_take_another_users_name() {
    let (store, secrets, _) = setup();
    store.add_user(&UserName::new("ann").unwrap()).unwrap();
    assert!(matches!(
        secrets.create_owner(&UserName::new("ANN").unwrap(), "$argon2id$x"),
        Err(StoreError::UserName(_))
    ));
    assert!(!secrets.setup_done().unwrap(), "nothing was written");
}

#[test]
fn a_refresh_digest_finds_its_device_until_that_is_revoked_or_expires() {
    let (store, secrets, clock) = setup();
    let device = store.add_device(UserId::OWNER, "desktop", 5_000).unwrap();
    assert!(matches!(
        secrets.put_session(device.id, &[1; 4]),
        Err(StoreError::InviteInvalid)
    ));
    secrets
        .put_session(device.id, &[9; MIN_INVITE_DIGEST_BYTES])
        .unwrap();
    secrets.put_session(device.id, &[1; 32]).unwrap();
    let found = secrets.session_for(&[1; 32]).unwrap().unwrap();
    assert_eq!(found.device, device.id);
    assert_eq!(found.user, UserId::OWNER);
    assert_eq!(found.role, Role::Owner);
    assert_eq!(secrets.session_for(&[2; 32]).unwrap(), None);
    // A new token replaces the old one.
    secrets.put_session(device.id, &[3; 32]).unwrap();
    assert_eq!(secrets.session_for(&[1; 32]).unwrap(), None);
    assert!(secrets.session_for(&[3; 32]).unwrap().is_some());
    clock.set(5_000);
    assert_eq!(secrets.session_for(&[3; 32]).unwrap(), None, "expired");
    clock.set(1_000);
    store.revoke_device(UserId::OWNER, device.id).unwrap();
    assert_eq!(secrets.session_for(&[3; 32]).unwrap(), None, "revoked");
}

#[test]
fn revoking_a_device_forgets_its_refresh_token_and_only_for_its_owner() {
    let (store, secrets, _) = setup();
    let ann = store.add_user(&UserName::new("ann").unwrap()).unwrap().id;
    let hers = store.add_device(ann, "hers", 5_000).unwrap();
    secrets.put_session(hers.id, &[1; 32]).unwrap();
    // Someone else's revoke changes nothing, and the token stays.
    assert!(!store.revoke_device(UserId::OWNER, hers.id).unwrap());
    assert!(secrets.session_for(&[1; 32]).unwrap().is_some());
    assert!(store.revoke_device(ann, hers.id).unwrap());
    // The token row is gone, not only hidden by the revoked flag.
    store.break_database("UPDATE devices SET revoked_at = NULL");
    assert_eq!(secrets.session_for(&[1; 32]).unwrap(), None);

    let mine = store.add_device(UserId::OWNER, "mine", 5_000).unwrap();
    secrets.put_session(mine.id, &[2; 32]).unwrap();
    assert_eq!(
        store.revoke_devices_of(UserId::OWNER, None).unwrap(),
        vec![mine.id]
    );
    store.break_database("UPDATE devices SET revoked_at = NULL");
    assert_eq!(secrets.session_for(&[2; 32]).unwrap(), None);
}

#[test]
fn a_failing_database_is_an_error_for_the_owner_steps() {
    let (store, secrets, _) = setup();
    store.break_database("ALTER TABLE account_passwords RENAME TO passwords_gone");
    assert!(secrets.has_password(UserId::OWNER).is_err());
    let (store, secrets, _) = setup();
    store.break_database("ALTER TABLE users RENAME TO users_gone");
    let name = UserName::new("Tijs").unwrap();
    assert!(secrets.create_owner(&name, "$argon2id$x").is_err());
}
