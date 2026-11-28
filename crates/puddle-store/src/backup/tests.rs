// SPDX-License-Identifier: AGPL-3.0-or-later
use std::sync::Arc;

use super::*;
use crate::{Limits, ManualClock, Store};

fn open(path: &Path) -> Store {
    Store::open(path, Arc::new(ManualClock::new(1_000)), Limits::default()).unwrap()
}

#[test]
fn copy_has_the_committed_rows_and_the_schema_version() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("puddle.db");
    let store = open(&src);
    let rules = store.rules_every_user(&crate::testing::all_users()).len();
    let dest = dir.path().join("copy.db");
    let info = backup_database(&src, &dest).unwrap();
    assert_eq!(info.schema_version, crate::SCHEMA_VERSION);
    let copy = open(&dest);
    assert_eq!(
        copy.rules_every_user(&crate::testing::all_users()).len(),
        rules
    );
    drop(copy);
    let beside_the_copy: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("copy.db"))
        .collect();
    assert_eq!(
        beside_the_copy,
        ["copy.db"],
        "the copy leaves no sidecar files"
    );
}

#[test]
fn existing_destination_is_refused_and_left_alone() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("puddle.db");
    drop(open(&src));
    let dest = dir.path().join("copy.db");
    std::fs::write(&dest, b"keep me").unwrap();
    assert!(backup_database(&src, &dest).is_err());
    assert_eq!(std::fs::read(&dest).unwrap(), b"keep me");
}

#[test]
fn missing_source_is_an_error_and_creates_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("absent.db");
    let dest = dir.path().join("copy.db");
    assert!(backup_database(&src, &dest).is_err());
    assert!(!src.exists(), "the source is never created");
    assert!(!dest.exists());
}

#[test]
fn destination_path_with_a_quote_works() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("puddle.db");
    drop(open(&src));
    let dest = dir.path().join("it's a copy.db");
    backup_database(&src, &dest).unwrap();
    assert!(dest.exists());
}

#[test]
fn a_failure_after_the_copy_removes_the_copy() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("puddle.db");
    drop(open(&src));
    Connection::open(&src)
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER keep_hashes BEFORE DELETE ON account_passwords
             BEGIN SELECT RAISE(ABORT, 'no'); END;
             INSERT INTO account_passwords (user_id, hash, changed_at)
             SELECT id, 'CANARY-hash', 0 FROM users;",
        )
        .unwrap();
    let dest = dir.path().join("copy.db");
    assert!(backup_database(&src, &dest).is_err());
    assert!(!dest.exists(), "a copy with password hashes was left");
}

#[cfg(unix)]
#[test]
fn a_destination_that_is_not_utf8_is_refused_and_creates_nothing() {
    use std::os::unix::ffi::OsStrExt;
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("puddle.db");
    drop(open(&src));
    let dest = dir
        .path()
        .join(std::ffi::OsStr::from_bytes(b"copy-\xff.db"));
    assert!(matches!(
        backup_database(&src, &dest),
        Err(StoreError::PathNotUtf8(_))
    ));
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[test]
fn integrity_answer_other_than_ok_is_corruption() {
    assert!(verdict("ok").is_ok());
    let err = verdict("row 3 missing from index i").unwrap_err();
    assert!(err.to_string().contains("integrity check"), "{err}");
}

#[test]
fn the_copy_keeps_the_users_and_none_of_their_sign_in_secrets() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("puddle.db");
    let store = open(&src);
    let secrets = store.take_account_secrets().unwrap();
    let owner = crate::users::UserId::OWNER;
    secrets
        .set_password_hash(owner, "$argon2id$CANARY-password-hash")
        .unwrap();
    secrets
        .put_invite(owner, b"CANARY-invite-digest-bytes", 9_999_999)
        .unwrap();
    let device = store.add_device(owner, "desktop", 5_000).unwrap();
    secrets
        .put_session(device.id, b"CANARY-refresh-digest-bytes-0123456789")
        .unwrap();
    let dest = dir.path().join("copy.db");
    backup_database(&src, &dest).unwrap();

    let bytes = std::fs::read(&dest).unwrap();
    for canary in [
        &b"CANARY-refresh-digest-bytes"[..],
        &b"CANARY-password-hash"[..],
        &b"CANARY-invite-digest-bytes"[..],
    ] {
        assert!(
            !bytes.windows(canary.len()).any(|w| w == canary),
            "the copy still holds {}",
            String::from_utf8_lossy(canary)
        );
    }
    let copy = open(&dest);
    assert_eq!(
        copy.users().unwrap(),
        store.users().unwrap(),
        "the users stay"
    );
    let copied_secrets = copy.take_account_secrets().unwrap();
    assert!(
        !copied_secrets.check_password(owner, |_| true).unwrap(),
        "the password is gone"
    );
    assert!(
        copied_secrets
            .redeem_invite(b"CANARY-invite-digest-bytes")
            .unwrap()
            .is_none()
    );
    assert!(
        copied_secrets
            .session_for(b"CANARY-refresh-digest-bytes-0123456789")
            .unwrap()
            .is_none(),
        "the session is gone"
    );
    assert!(
        secrets.check_password(owner, |_| true).unwrap(),
        "the live database keeps its own"
    );
    assert!(
        secrets
            .session_for(b"CANARY-refresh-digest-bytes-0123456789")
            .unwrap()
            .is_some(),
        "the live database keeps its session"
    );
}

#[test]
fn a_database_from_before_the_secret_tables_copies_as_it_is() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("old.db");
    let conn = Connection::open(&src).unwrap();
    conn.execute_batch("CREATE TABLE kept (x TEXT); INSERT INTO kept VALUES ('row');")
        .unwrap();
    drop(conn);
    let dest = dir.path().join("copy.db");
    backup_database(&src, &dest).unwrap();
    let copy = Connection::open(&dest).unwrap();
    let kept: String = copy
        .query_row("SELECT x FROM kept", [], |row| row.get(0))
        .unwrap();
    assert_eq!(kept, "row");
}
