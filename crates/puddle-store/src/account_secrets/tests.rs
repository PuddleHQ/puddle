// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;
use crate::clock::ManualClock;
use crate::store::{Limits, Store};
use crate::users::UserName;

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
