// SPDX-License-Identifier: AGPL-3.0-or-later
use puddle_store::{Limits, Store, SystemClock};
use sha2::Digest as _;

use super::*;

fn data_with_owner() -> (tempfile::TempDir, HostPaths) {
    let dir = tempfile::tempdir().unwrap();
    let paths = HostPaths::new(dir.path().to_path_buf());
    let store = Store::open(&paths.store(), Arc::new(SystemClock), Limits::default()).unwrap();
    drop(store);
    (dir, paths)
}

#[test]
fn a_reset_makes_an_invite_for_a_user_that_exists_and_says_so_for_one_that_does_not() {
    let (_dir, paths) = data_with_owner();
    let invite = reset_user(&paths, "OWNER").unwrap();
    assert_eq!(invite.secret.len(), 43);
    // The invite is in the database, as a digest the API redeems.
    let store = Store::open(&paths.store(), Arc::new(SystemClock), Limits::default()).unwrap();
    let secrets = store.take_account_secrets().unwrap();
    let digest: [u8; 32] = sha2::Sha256::digest(invite.secret.as_bytes()).into();
    assert_eq!(secrets.redeem_invite(&digest).unwrap(), Some(UserId::OWNER));
    assert!(matches!(
        reset_user(&paths, "ghost"),
        Err(AccountError::Reset(ResetError::UnknownUser(name))) if name == "ghost"
    ));
}

#[test]
fn a_folder_without_a_database_is_not_made_into_one() {
    let dir = tempfile::tempdir().unwrap();
    let paths = HostPaths::new(dir.path().to_path_buf());
    assert!(matches!(
        reset_user(&paths, "owner"),
        Err(AccountError::NoDatabase(_))
    ));
    assert!(!paths.store().exists());
}

fn reopen(paths: &HostPaths) -> Store {
    Store::open(&paths.store(), Arc::new(SystemClock), Limits::default()).unwrap()
}

#[test]
fn an_added_user_is_invited_listed_and_audited_and_a_taken_name_is_refused() {
    let (_dir, paths) = data_with_owner();
    let invite = add_user(&paths, "Ann").unwrap();
    assert_eq!(invite.secret.len(), 43);
    let store = reopen(&paths);
    let secrets = store.take_account_secrets().unwrap();
    let digest: [u8; 32] = sha2::Sha256::digest(invite.secret.as_bytes()).into();
    let ann = store.user_named("Ann").unwrap().unwrap();
    assert_eq!(secrets.redeem_invite(&digest).unwrap(), Some(ann.id));
    assert!(!secrets.has_password(ann.id).unwrap());
    drop(store);

    let rows = list_users(&paths).unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows[0].owner && rows[0].state == UserState::Invited);
    assert_eq!(
        (rows[1].name.as_str(), rows[1].owner, rows[1].state),
        ("Ann", false, UserState::Invited)
    );
    assert_eq!((rows[1].devices, rows[1].last_sign_in), (0, None));

    assert!(matches!(
        add_user(&paths, "ann"),
        Err(AccountError::Store(puddle_store::StoreError::UserName(_)))
    ));
    assert!(matches!(
        add_user(&paths, ""),
        Err(AccountError::Store(puddle_store::StoreError::UserName(_)))
    ));
    let lines = reopen(&paths).admin_audit(10).unwrap();
    assert_eq!(lines.len(), 1, "a refused add leaves no line");
}

#[test]
fn a_removed_user_starts_deleting_and_loses_every_way_in_and_the_owner_stays() {
    let (_dir, paths) = data_with_owner();
    add_user(&paths, "Ann").unwrap();
    let store = reopen(&paths);
    let ann = store.user_named("Ann").unwrap().unwrap().id;
    let secrets = store.take_account_secrets().unwrap();
    secrets.put_invite(ann, &[7; 32], u64::MAX).unwrap();
    drop(store);

    remove_user(&paths, "Ann").unwrap();
    let rows = list_users(&paths).unwrap();
    assert_eq!(rows[1].state, UserState::Deleting);
    let store = reopen(&paths);
    assert_eq!(store.users_deleting().unwrap().len(), 1);
    let secrets = store.take_account_secrets().unwrap();
    assert!(!secrets.has_password(ann).unwrap());
    assert_eq!(secrets.redeem_invite(&[7; 32]).unwrap(), None);
    drop(store);

    assert!(matches!(
        remove_user(&paths, "owner"),
        Err(AccountError::Store(puddle_store::StoreError::OwnerKept))
    ));
    assert!(matches!(
        remove_user(&paths, "ghost"),
        Err(AccountError::UnknownUser(name)) if name == "ghost"
    ));
}

#[test]
fn listing_adding_and_removing_need_a_database() {
    let dir = tempfile::tempdir().unwrap();
    let paths = HostPaths::new(dir.path().to_path_buf());
    assert!(matches!(
        list_users(&paths),
        Err(AccountError::NoDatabase(_))
    ));
    assert!(matches!(
        add_user(&paths, "ann"),
        Err(AccountError::NoDatabase(_))
    ));
    assert!(matches!(
        remove_user(&paths, "ann"),
        Err(AccountError::NoDatabase(_))
    ));
    assert!(!paths.store().exists());
}

#[test]
fn a_user_who_chose_a_password_is_listed_as_active() {
    let (_dir, paths) = data_with_owner();
    add_user(&paths, "Ann").unwrap();
    let store = reopen(&paths);
    let ann = store.user_named("Ann").unwrap().unwrap();
    store
        .take_account_secrets()
        .unwrap()
        .set_password_hash(ann.id, "a stored hash")
        .unwrap();
    drop(store);
    let rows = list_users(&paths).unwrap();
    assert_eq!(
        (rows[1].name.as_str(), rows[1].state),
        ("Ann", UserState::Active)
    );
    assert_eq!(rows[0].state, UserState::Invited);
}
