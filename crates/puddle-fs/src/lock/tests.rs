// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn a_second_lock_on_the_same_folder_is_refused_until_the_first_is_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let first = DataLock::acquire(dir.path()).unwrap();
    let err = DataLock::acquire(dir.path()).unwrap_err();
    // Same process here, so the note names this one.
    assert!(
        matches!(&err, LockError::Held { holder, .. } if *holder == Some(std::process::id())),
        "{err}"
    );
    assert!(
        err.to_string()
            .contains(&format!("process ID {}", std::process::id())),
        "{err}"
    );
    drop(first);
    drop(DataLock::acquire(dir.path()).unwrap());
}

#[test]
fn different_folders_do_not_block_each_other() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let _a = DataLock::acquire(a.path()).unwrap();
    let _b = DataLock::acquire(b.path()).unwrap();
}

#[test]
fn a_missing_folder_is_created() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("new").join("puddle");
    let _lock = DataLock::acquire(&data).unwrap();
    assert!(data.join(LOCK_FILE).is_file());
}

#[test]
fn a_leftover_lock_file_and_note_do_not_block_and_are_replaced() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(LOCK_FILE), b"").unwrap();
    std::fs::write(dir.path().join(HOLDER_FILE), b"4000000\n").unwrap();
    let _lock = DataLock::acquire(dir.path()).unwrap();
    assert_eq!(
        read_holder(&dir.path().join(HOLDER_FILE)),
        Some(std::process::id())
    );
}

#[test]
fn a_refusal_without_a_readable_note_names_no_process() {
    let dir = tempfile::tempdir().unwrap();
    let _first = DataLock::acquire(dir.path()).unwrap();
    std::fs::remove_file(dir.path().join(HOLDER_FILE)).unwrap();
    let err = DataLock::acquire(dir.path()).unwrap_err();
    assert!(matches!(err, LockError::Held { holder: None, .. }), "{err}");
    assert!(
        err.to_string().contains("another Puddle process is"),
        "{err}"
    );
}

#[test]
fn a_data_folder_that_is_a_file_is_an_io_error() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("f");
    std::fs::write(&file, b"x").unwrap();
    let err = DataLock::acquire(&file).unwrap_err();
    assert!(matches!(err, LockError::Io { .. }), "{err}");
    assert!(err.to_string().contains("cannot lock"), "{err}");
}

#[test]
fn a_lock_path_that_cannot_be_opened_is_an_io_error() {
    let dir = tempfile::tempdir().unwrap();
    // A folder where the lock file should be: it exists, and opening it for writing fails.
    std::fs::create_dir(dir.path().join(LOCK_FILE)).unwrap();
    let err = DataLock::acquire(dir.path()).unwrap_err();
    assert!(matches!(err, LockError::Io { .. }), "{err}");
}

#[test]
fn a_lock_file_that_cannot_be_created_reports_why() {
    let dir = tempfile::tempdir().unwrap();
    // The folder is missing, so creating the file fails for a reason other than "exists".
    let err = open_lock_file(&dir.path().join("missing").join(LOCK_FILE)).unwrap_err();
    assert_ne!(err.kind(), io::ErrorKind::AlreadyExists, "{err}");
}

#[cfg(unix)]
#[test]
fn a_lock_file_that_cannot_be_created_is_not_retried_as_an_existing_one() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
    // A user who may write in it anyway (root) gets no error at all.
    let can_write = File::create(dir.path().join("probe")).is_ok();
    let kind = open_lock_file(&dir.path().join(LOCK_FILE))
        .err()
        .map(|e| e.kind());
    // Opening the missing file instead would say it is not found, hiding why it was not made.
    assert_eq!(
        kind,
        (!can_write).then_some(io::ErrorKind::PermissionDenied)
    );
}

#[test]
fn a_lock_call_the_system_refuses_for_another_reason_is_an_io_error_naming_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let lock_path = dir.path().join(LOCK_FILE);
    let refused = TryLockError::Error(io::Error::from(io::ErrorKind::Unsupported));
    let err = lock_outcome(Err(refused), dir.path(), &lock_path).unwrap_err();
    assert!(
        matches!(&err, LockError::Io { path, source }
            if *path == lock_path && source.kind() == io::ErrorKind::Unsupported),
        "{err}"
    );
    assert!(
        err.to_string().contains(&lock_path.display().to_string()),
        "{err}"
    );
}

#[test]
fn a_lock_held_elsewhere_names_the_holder_from_its_note() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(HOLDER_FILE), "4242\n").unwrap();
    let err = lock_outcome(
        Err(TryLockError::WouldBlock),
        dir.path(),
        &dir.path().join(LOCK_FILE),
    )
    .unwrap_err();
    assert!(
        matches!(
            &err,
            LockError::Held {
                holder: Some(4242),
                ..
            }
        ),
        "{err}"
    );
    assert!(lock_outcome(Ok(()), dir.path(), &dir.path().join(LOCK_FILE)).is_ok());
}
