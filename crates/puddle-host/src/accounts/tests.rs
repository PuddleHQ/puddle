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
    assert_eq!(
        secrets.redeem_invite(&digest).unwrap(),
        Some(puddle_store::UserId::OWNER)
    );
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
