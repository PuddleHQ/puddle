// SPDX-License-Identifier: AGPL-3.0-or-later
//! The ACL as the OS reports it, and other principals trying to open the file.

use windows_sys::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE};
use windows_sys::Win32::Storage::FileSystem::FILE_ALL_ACCESS;

use super::*;
use crate::win::testing::{
    SecondUser, SharedDir, assert_access_denied, assert_owner_only_dacl, open_restricted,
};

#[test]
fn a_new_file_has_one_protected_ace_for_the_owner() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("f");
    drop(create_file(&path).unwrap());
    let file = File::open(&path).unwrap();
    assert_owner_only_dacl(&file, FILE_ALL_ACCESS);
    check(&file).unwrap();
}

#[test]
fn a_new_folder_has_the_owner_ace_and_so_do_its_missing_parents() {
    let dir = tempfile::tempdir().unwrap();
    let leaf = dir.path().join("a").join("b");
    create_dir(&leaf).unwrap();
    for folder in [dir.path().join("a"), leaf.clone()] {
        let handle = OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(&folder)
            .unwrap();
        assert_owner_only_dacl(&handle, FILE_ALL_ACCESS);
    }
    // An existing folder keeps the ACL it has (the temporary folder inherits from the profile).
    create_dir(dir.path()).unwrap();
    // A file created in an owner-only folder by other means gets the owner-only entry too.
    let inner = leaf.join("plain");
    fs::write(&inner, b"x").unwrap();
    check(&File::open(&inner).unwrap()).unwrap();
}

#[test]
fn check_refuses_a_file_with_an_inherited_acl() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("plain");
    fs::write(&path, b"x").unwrap();
    let err = check(&File::open(&path).unwrap()).unwrap_err();
    assert!(matches!(err, CheckError::Exposed(Exposed::Acl(_))), "{err}");
}

#[test]
fn a_restricted_token_without_the_user_sid_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("f");
    drop(create_file(&path).unwrap());
    assert_access_denied(open_restricted(&path, GENERIC_READ | GENERIC_WRITE, false));
    assert_access_denied(open_restricted(&path, GENERIC_READ, false));
}

#[test]
fn the_same_restricted_token_with_the_user_sid_gets_in() {
    // Positive control: the refusal above comes from the ACL, not from the token setup.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("f");
    drop(create_file(&path).unwrap());
    open_restricted(&path, GENERIC_READ | GENERIC_WRITE, true).unwrap();
}

#[test]
fn a_folder_with_an_inherited_acl_is_tightened_and_its_children_follow() {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("data");
    fs::create_dir(&folder).unwrap();
    let child = folder.join("puddle.db");
    fs::write(&child, b"x").unwrap();
    // The temporary folder inherits from the profile, so both are open to more than us.
    assert!(check(&File::open(&child).unwrap()).is_err());
    let was = tighten_dir(&folder).unwrap();
    assert!(matches!(was, Some(Exposed::Acl(_))), "{was:?}");
    let handle = OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(&folder)
        .unwrap();
    assert_owner_only_dacl(&handle, FILE_ALL_ACCESS);
    // The existing child picked up the folder's one entry.
    check(&File::open(&child).unwrap()).unwrap();
    assert_eq!(tighten_dir(&folder).unwrap(), None);
    assert_eq!(fs::read(&child).unwrap(), b"x");
}

#[test]
fn a_file_with_an_inherited_acl_is_tightened_alone() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("puddle.db");
    fs::write(&path, b"x").unwrap();
    let was = tighten_file(&path).unwrap();
    assert!(matches!(was, Some(Exposed::Acl(_))), "{was:?}");
    assert_owner_only_dacl(&File::open(&path).unwrap(), FILE_ALL_ACCESS);
    assert_eq!(tighten_file(&path).unwrap(), None);
}

/// A file that is deleted but still open elsewhere, as SQLite leaves a `-wal` or `-shm` file
/// while a connection closes. Returns the handle that keeps it pending.
fn delete_pending(path: &Path) -> File {
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_DELETE_ON_CLOSE, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };
    let share = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;
    let keeper = OpenOptions::new()
        .read(true)
        .share_mode(share)
        .open(path)
        .unwrap();
    // Closing the handle that asked for delete-on-close deletes the file, but the file stays
    // (delete pending) until `keeper` closes too.
    drop(
        OpenOptions::new()
            .read(true)
            .share_mode(share)
            .custom_flags(FILE_FLAG_DELETE_ON_CLOSE)
            .open(path)
            .unwrap(),
    );
    keeper
}

#[test]
fn a_file_whose_delete_is_pending_reads_as_denied_not_missing() {
    // The premise of the retry in `private::tighten_file`.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("puddle.db-wal");
    fs::write(&path, b"x").unwrap();
    let keeper = delete_pending(&path);
    let err = tighten_file(&path).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::PermissionDenied, "{err}");
    drop(keeper);
    assert_eq!(
        tighten_file(&path).unwrap_err().kind(),
        io::ErrorKind::NotFound
    );
}

#[test]
fn a_file_deleted_while_it_is_tightened_is_nothing_to_tighten() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("puddle.db-wal");
    fs::write(&path, b"x").unwrap();
    let keeper = delete_pending(&path);
    let (denied_once, denied_once_rx) = std::sync::mpsc::channel();
    let closer = std::thread::spawn(move || {
        // The pending delete completes only after the tighten step was denied once, so it is retrying.
        denied_once_rx.recv().unwrap();
        drop(keeper);
    });
    let result = crate::private::tighten_file_within(crate::private::DELETE_GRACE, || {
        let result = tighten_file(&path);
        if result.is_err() {
            let _ = denied_once.send(());
        }
        result
    });
    assert_eq!(result.unwrap(), None);
    closer.join().unwrap();
}

#[test]
fn a_tightened_file_refuses_a_restricted_token() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("puddle.db");
    fs::write(&path, b"x").unwrap();
    tighten_file(&path).unwrap();
    assert_access_denied(open_restricted(&path, GENERIC_READ, false));
}

#[test]
fn another_local_user_is_refused() {
    // Fails instead of skipping where `PUDDLE_REQUIRE_SECOND_USER=1` (the Windows workflows).
    let Some(user) = SecondUser::for_test() else {
        return;
    };
    let dir = SharedDir::new("puddle-fs");
    let plain = dir.path().join("plain");
    fs::write(&plain, b"x").unwrap();
    let private = dir.path().join("private");
    drop(create_file(&private).unwrap());

    // Control: the account can read an ordinary file there, so the refusals below come from
    // the owner-only ACL and nothing else.
    user.open(&plain, GENERIC_READ).unwrap();
    assert_access_denied(user.open(&private, GENERIC_READ));
    assert_access_denied(user.open(&private, GENERIC_WRITE));
    assert_access_denied(user.open(&private, GENERIC_READ | GENERIC_WRITE));
}
