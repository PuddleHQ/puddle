// SPDX-License-Identifier: GPL-3.0-or-later
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
    let rules = store.rules().len();
    let dest = dir.path().join("copy.db");
    let info = backup_database(&src, &dest).unwrap();
    assert_eq!(info.schema_version, crate::SCHEMA_VERSION);
    let copy = open(&dest);
    assert_eq!(copy.rules().len(), rules);
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
    let dest = dir.path().join("copy.db");
    backup_database(&src, &dest).unwrap();

    let bytes = std::fs::read(&dest).unwrap();
    for canary in [
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
        secrets.check_password(owner, |_| true).unwrap(),
        "the live database keeps its own"
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
