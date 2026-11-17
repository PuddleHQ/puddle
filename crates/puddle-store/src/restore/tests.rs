// SPDX-License-Identifier: AGPL-3.0-or-later
use std::sync::Arc;

use super::*;
use crate::{Limits, ManualClock, Store};

fn open(path: &Path) {
    drop(Store::open(path, Arc::new(ManualClock::new(1_000)), Limits::default()).unwrap());
}

fn sql(path: &Path, statements: &str) {
    Connection::open(path)
        .unwrap()
        .execute_batch(statements)
        .unwrap();
}

fn rule(host: &str, effect: &str, precedence: u8) -> String {
    format!(
        "INSERT INTO rules (scope, pattern_kind, pattern, effect, created_at, created_by, user_id, precedence) \
         VALUES ('global', 'exact', '{host}', '{effect}', 1, 'cli', 1, {precedence});"
    )
}

#[test]
fn a_file_that_is_not_a_database_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("puddle.db");
    std::fs::write(&path, vec![b'x'; 4096]).unwrap();
    let refusal = check_staged_database(&path).unwrap().unwrap_err();
    assert!(
        matches!(refusal, DatabaseRefusal::NotPuddle(_)),
        "{refusal}"
    );
}

#[test]
fn a_database_that_is_not_puddles_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("puddle.db");
    sql(&path, "CREATE TABLE other (x INTEGER);");
    let refusal = check_staged_database(&path).unwrap().unwrap_err();
    assert!(
        matches!(refusal, DatabaseRefusal::NotPuddle(_)),
        "{refusal}"
    );
}

#[test]
fn a_newer_schema_is_refused_naming_both_versions() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("puddle.db");
    open(&path);
    sql(
        &path,
        &format!("PRAGMA user_version = {};", SCHEMA_VERSION + 1),
    );
    let refusal = check_staged_database(&path).unwrap().unwrap_err();
    assert_eq!(
        refusal,
        DatabaseRefusal::NewerSchema {
            found: SCHEMA_VERSION + 1,
            supported: SCHEMA_VERSION
        }
    );
    let text = refusal.to_string();
    assert!(
        text.contains(&format!("schema {}", SCHEMA_VERSION + 1)),
        "{text}"
    );
    assert!(text.contains("update Puddle first"), "{text}");
}

#[test]
fn a_damaged_database_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("puddle.db");
    open(&path);
    for n in 0..300 {
        sql(&path, &rule(&format!("h{n}.example.org"), "allow", 5));
    }
    sql(&path, "PRAGMA wal_checkpoint(TRUNCATE);");
    let mut bytes = std::fs::read(&path).unwrap();
    // A page in the middle of the file: the header stays readable, the content does not.
    let mid = (bytes.len() / 2) & !4095;
    bytes[mid..mid + 4096].fill(0xff);
    std::fs::write(&path, bytes).unwrap();
    let refusal = check_staged_database(&path).unwrap().unwrap_err();
    assert!(
        matches!(
            refusal,
            DatabaseRefusal::Integrity(_) | DatabaseRefusal::NotPuddle(_)
        ),
        "{refusal}"
    );
}

#[test]
fn an_older_schema_is_accepted_and_migrated_forward() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("puddle.db");
    {
        let mut conn = Connection::open(&path).unwrap();
        schema::migrate_up_to(&mut conn, 20).unwrap();
    }
    let staged = check_staged_database(&path).unwrap().unwrap();
    assert_eq!(staged.schema_version, 20);
    let version: u32 = Connection::open(&path)
        .unwrap()
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
}

#[test]
fn preview_counts_what_each_mode_would_do() {
    let dir = tempfile::tempdir().unwrap();
    let here = dir.path().join("here.db");
    let there = dir.path().join("there.db");
    open(&here);
    open(&there);
    // Here: a, b, c. Backup: a (same), b (a different precedence), d (new).
    sql(
        &here,
        &[
            rule("a.example.org", "allow", 5),
            rule("b.example.org", "allow", 5),
            rule("c.example.org", "allow", 5),
        ]
        .concat(),
    );
    sql(
        &there,
        &[
            rule("a.example.org", "allow", 5),
            rule("b.example.org", "allow", 7),
            rule("d.example.org", "allow", 5),
        ]
        .concat(),
    );
    // A secret variable in the backup, and a user that only the backup has.
    sql(
        &there,
        "INSERT INTO env_vars (workspace_id, name, kind, secret_id, hosts, changed_at, user_id) \
         VALUES ('w1', 'TOKEN', 'secret', 'sid-1', 'example.org', 1, 1);
         INSERT INTO users (role, name, created_at) VALUES ('user', 'Ann', 1);",
    );
    let before = std::fs::read(&here).unwrap();
    let preview = preview_merge(&here, &there).unwrap();
    assert_eq!(
        preview.rules,
        PartCounts {
            in_backup: 3,
            add: 1,
            differ: 1,
            same: 1,
            only_here: 1
        }
    );
    assert_eq!(
        preview.environment,
        PartCounts {
            in_backup: 1,
            add: 1,
            differ: 0,
            same: 0,
            only_here: 0
        }
    );
    assert_eq!(
        preview.users,
        PartCounts {
            in_backup: 2,
            add: 1,
            differ: 0,
            same: 1,
            only_here: 0
        }
    );
    assert_eq!(
        preview.secret_envs,
        vec![SecretEnv {
            workspace_id: Some("w1".into()),
            name: "TOKEN".into(),
            secret_id: "sid-1".into()
        }]
    );
    assert_eq!(
        std::fs::read(&here).unwrap(),
        before,
        "the present file is untouched"
    );
}

#[test]
fn the_backups_owner_matches_this_machines_owner_whatever_its_name() {
    let dir = tempfile::tempdir().unwrap();
    let here = dir.path().join("here.db");
    let there = dir.path().join("there.db");
    open(&here);
    open(&there);
    sql(
        &there,
        "UPDATE users SET name = 'Old Laptop Owner' WHERE role = 'owner';",
    );
    sql(&there, &rule("a.example.org", "allow", 5));
    sql(&here, &rule("a.example.org", "allow", 5));
    let preview = preview_merge(&here, &there).unwrap();
    assert_eq!(preview.users.same, 1);
    assert_eq!(preview.users.add, 0);
    assert_eq!(preview.rules.same, 1);
}
