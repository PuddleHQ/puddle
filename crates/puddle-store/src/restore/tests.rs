// SPDX-License-Identifier: AGPL-3.0-or-later
use std::path::PathBuf;
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
        "UPDATE users SET name = 'Old Computer Owner' WHERE role = 'owner';",
    );
    sql(&there, &rule("a.example.org", "allow", 5));
    sql(&here, &rule("a.example.org", "allow", 5));
    let preview = preview_merge(&here, &there).unwrap();
    assert_eq!(preview.users.same, 1);
    assert_eq!(preview.users.add, 0);
    assert_eq!(preview.rules.same, 1);
}

fn count(path: &Path, sql: &str) -> i64 {
    Connection::open(path)
        .unwrap()
        .query_row(sql, [], |r| r.get(0))
        .unwrap()
}

/// A present database and a backup that overlap: see the rows set below.
fn pair(dir: &Path) -> (PathBuf, PathBuf) {
    let here = dir.join("here.db");
    let there = dir.join("there.db");
    open(&here);
    open(&there);
    sql(
        &here,
        &[
            rule("a.example.org", "allow", 5),
            rule("b.example.org", "allow", 5),
            rule("only-here.example.org", "allow", 5),
        ]
        .concat(),
    );
    sql(
        &there,
        &[
            rule("a.example.org", "allow", 5),
            rule("b.example.org", "allow", 8),
            rule("new.example.org", "deny", 5),
        ]
        .concat(),
    );
    // A rule set with a rule in it, an identity, a workspace's row, a pending request, a user.
    sql(
        &there,
        "INSERT INTO rule_sets (name, description, created_at, created_by, user_id) VALUES ('Mine', '', 1, 'cli', 1);
         INSERT INTO rules (scope, set_id, pattern_kind, pattern, effect, created_at, created_by, user_id)
             VALUES ('set', (SELECT id FROM rule_sets WHERE name = 'Mine'), 'exact', 'in-set.example.org', 'allow', 1, 'cli', 1);
         INSERT INTO identities (user_id, label, author_name, author_email, credentials, position, is_default, created_at, changed_at)
             VALUES (1, 'work', 'A', 'a@example.org', '{}', 0, 1, 1, 1);
         INSERT INTO workspace_names (workspace_id, name) VALUES ('w1', 'One');
         INSERT INTO pending (workspace_id, host, port, first_seen, last_seen, attempts, state)
             VALUES ('w1', 'wait.example.org', 443, 1, 1, 1, 'requested');
         INSERT INTO users (role, name, created_at) VALUES ('user', 'Ann', 1);
         INSERT INTO rules (scope, pattern_kind, pattern, effect, created_at, created_by, user_id)
             VALUES ('global', 'exact', 'ann.example.org', 'allow', 1, 'cli', (SELECT id FROM users WHERE name = 'Ann'));
         INSERT INTO account_passwords (user_id, hash, changed_at) VALUES ((SELECT id FROM users WHERE name = 'Ann'), 'HASH', 1);",
    );
    sql(
        &here,
        "INSERT INTO identities (user_id, label, author_name, author_email, credentials, position, is_default, created_at, changed_at)
             VALUES (1, 'mine', 'H', 'h@example.org', '{}', 0, 1, 1, 1);",
    );
    (here, there)
}

fn hosts(path: &Path) -> Vec<String> {
    let conn = Connection::open(path).unwrap();
    let mut stmt = conn
        .prepare("SELECT pattern || ':' || effect || ':' || precedence FROM rules ORDER BY pattern")
        .unwrap();
    stmt.query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

#[test]
fn add_keeps_what_is_here_and_adds_what_is_missing() {
    let dir = tempfile::tempdir().unwrap();
    let (here, there) = pair(dir.path());
    let dest = dir.path().join("new.db");
    let outcome = restore_database(&here, &there, &dest, RestoreMode::Add).unwrap();
    assert_eq!(
        hosts(&dest),
        [
            "a.example.org:allow:5",
            "ann.example.org:allow:5",
            // yours is kept as it is: the backup's precedence 8 is not taken
            "b.example.org:allow:5",
            "in-set.example.org:allow:5",
            "new.example.org:deny:5",
            "only-here.example.org:allow:5",
        ]
    );
    // The restored rule stays in its (restored) set.
    assert_eq!(
        count(
            &dest,
            "SELECT count(*) FROM rules r JOIN rule_sets s ON s.id = r.set_id WHERE s.name = 'Mine'"
        ),
        1
    );
    // The present default identity stays the only default; the backup's is added without being one.
    assert_eq!(count(&dest, "SELECT count(*) FROM identities"), 2);
    assert_eq!(
        count(
            &dest,
            "SELECT count(*) FROM identities WHERE is_default = 1 AND label = 'mine'"
        ),
        1
    );
    assert_eq!(count(&dest, "SELECT count(*) FROM workspace_names"), 1);
    // Pending requests are never restored.
    assert_eq!(count(&dest, "SELECT count(*) FROM pending"), 0);
    // Ann is added without a password, and her rule is hers.
    assert_eq!(
        count(&dest, "SELECT count(*) FROM users WHERE name = 'Ann'"),
        1
    );
    assert_eq!(count(&dest, "SELECT count(*) FROM account_passwords"), 0);
    assert_eq!(
        count(
            &dest,
            "SELECT count(*) FROM rules r JOIN users u ON u.id = r.user_id WHERE u.name = 'Ann'"
        ),
        1
    );
    assert_eq!(outcome.user_map.len(), 2);
    // The inputs are as they were.
    assert_eq!(count(&there, "SELECT count(*) FROM pending"), 1);
}

#[test]
fn replace_makes_the_carried_rows_the_backups_and_keeps_the_people() {
    let dir = tempfile::tempdir().unwrap();
    let (here, there) = pair(dir.path());
    let dest = dir.path().join("new.db");
    restore_database(&here, &there, &dest, RestoreMode::Replace).unwrap();
    assert_eq!(
        hosts(&dest),
        [
            "a.example.org:allow:5",
            "ann.example.org:allow:5",
            "b.example.org:allow:8",
            "in-set.example.org:allow:5",
            "new.example.org:deny:5",
        ]
    );
    assert_eq!(
        count(
            &dest,
            "SELECT count(*) FROM identities WHERE label = 'mine'"
        ),
        0
    );
    assert_eq!(
        count(
            &dest,
            "SELECT count(*) FROM identities WHERE label = 'work' AND is_default = 1"
        ),
        1
    );
    // The owner is still there, never replaced.
    assert_eq!(
        count(&dest, "SELECT count(*) FROM users WHERE role = 'owner'"),
        1
    );
}

#[test]
fn a_users_password_here_survives_a_restore_of_the_same_name() {
    let dir = tempfile::tempdir().unwrap();
    let (here, there) = pair(dir.path());
    sql(
        &here,
        "INSERT INTO users (role, name, created_at) VALUES ('user', 'ann', 1);
         INSERT INTO account_passwords (user_id, hash, changed_at) VALUES ((SELECT id FROM users WHERE name = 'ann'), 'MINE', 1);",
    );
    for mode in [RestoreMode::Add, RestoreMode::Replace] {
        let dest = dir.path().join(format!("new-{mode:?}.db"));
        restore_database(&here, &there, &dest, mode).unwrap();
        // Matched by name, whatever the case: no second Ann, and her password is hers.
        assert_eq!(
            count(
                &dest,
                "SELECT count(*) FROM users WHERE name = 'Ann' COLLATE NOCASE"
            ),
            1
        );
        assert_eq!(
            count(
                &dest,
                "SELECT count(*) FROM account_passwords WHERE hash = 'MINE'"
            ),
            1
        );
        assert_eq!(
            count(
                &dest,
                "SELECT count(*) FROM rules r JOIN users u ON u.id = r.user_id WHERE u.name = 'ann'"
            ),
            1,
            "{mode:?}: the backup's rule of Ann is now this machine's Ann's"
        );
    }
}

#[test]
fn the_audit_log_is_loaded_only_into_an_empty_log() {
    let dir = tempfile::tempdir().unwrap();
    let (here, there) = pair(dir.path());
    let line = "INSERT INTO audit (ts, type, line, user_id) VALUES (1, 'x', '{}', 1);";
    sql(&there, &[line, line].concat());
    let dest = dir.path().join("empty.db");
    restore_database(&here, &there, &dest, RestoreMode::Add).unwrap();
    assert_eq!(
        count(&dest, "SELECT count(*) FROM audit"),
        count(&there, "SELECT count(*) FROM audit")
    );

    sql(&here, line);
    let dest = dir.path().join("full.db");
    restore_database(&here, &there, &dest, RestoreMode::Replace).unwrap();
    assert_eq!(
        count(&dest, "SELECT count(*) FROM audit"),
        count(&here, "SELECT count(*) FROM audit")
    );
}

#[test]
fn a_failed_merge_leaves_no_new_file_and_the_present_database_intact() {
    let dir = tempfile::tempdir().unwrap();
    let (here, there) = pair(dir.path());
    // A backup whose users table is gone: the merge cannot map anyone.
    sql(
        &there,
        "PRAGMA legacy_alter_table = ON; ALTER TABLE users RENAME TO users_gone;",
    );
    let dest = dir.path().join("new.db");
    assert!(restore_database(&here, &there, &dest, RestoreMode::Add).is_err());
    assert!(!dest.exists());
    assert_eq!(count(&here, "SELECT count(*) FROM rules"), 3);
}

#[test]
fn a_path_that_cannot_be_opened_as_a_database_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    // A folder where the database should be.
    let refusal = check_staged_database(dir.path()).unwrap().unwrap_err();
    assert_eq!(
        refusal,
        DatabaseRefusal::NotPuddle("it cannot be opened".into())
    );
}

#[test]
fn a_backup_missing_a_table_this_schema_has_adds_nothing_from_it() {
    let dir = tempfile::tempdir().unwrap();
    let (here, there) = pair(dir.path());
    sql(&there, "DROP TABLE never_decrypt_global;");
    sql(
        &here,
        "INSERT INTO never_decrypt_global (pattern) VALUES ('mine.example.org');",
    );
    let dest = dir.path().join("new.db");
    restore_database(&here, &there, &dest, RestoreMode::Add).unwrap();
    assert_eq!(count(&dest, "SELECT count(*) FROM never_decrypt_global"), 1);
}

#[test]
fn a_present_database_with_a_dangling_reference_is_not_restored_over() {
    let dir = tempfile::tempdir().unwrap();
    let (here, there) = pair(dir.path());
    // A rule that names a rule set that is not there, as a damaged file might hold.
    sql(
        &here,
        "PRAGMA foreign_keys = OFF;
         INSERT INTO rules (scope, set_id, pattern_kind, pattern, effect, created_at, created_by, user_id)
             VALUES ('set', 999, 'exact', 'dangling.example.org', 'allow', 1, 'cli', 1);",
    );
    let dest = dir.path().join("new.db");
    let err = restore_database(&here, &there, &dest, RestoreMode::Add).unwrap_err();
    assert!(
        err.to_string().contains("without their parent row"),
        "{err}"
    );
    assert!(!dest.exists());
}

#[test]
fn a_backup_with_an_identity_and_stand_ins_keeps_them_attached_to_their_owner_rows() {
    let dir = tempfile::tempdir().unwrap();
    let (here, there) = pair(dir.path());
    sql(
        &there,
        "INSERT INTO env_vars (workspace_id, name, kind, value, changed_at, user_id)
             VALUES ('w1', 'PLAIN', 'plain', 'v', 1, 1);
         INSERT INTO env_stand_ins (workspace_id, env_id, stand_in)
             VALUES ('w1', (SELECT id FROM env_vars WHERE name = 'PLAIN'), 'standin');
         INSERT INTO workspace_identities (workspace_id, identity_id, position)
             VALUES ('w1', (SELECT id FROM identities WHERE label = 'work'), 0);",
    );
    let dest = dir.path().join("new.db");
    restore_database(&here, &there, &dest, RestoreMode::Add).unwrap();
    assert_eq!(
        count(
            &dest,
            "SELECT count(*) FROM env_stand_ins s JOIN env_vars e ON e.id = s.env_id WHERE e.name = 'PLAIN'"
        ),
        1
    );
    assert_eq!(
        count(
            &dest,
            "SELECT count(*) FROM workspace_identities w JOIN identities i ON i.id = w.identity_id WHERE i.label = 'work'"
        ),
        1
    );
}

#[test]
fn an_integrity_answer_other_than_ok_is_a_corruption_error_that_quotes_it() {
    assert!(verdict("ok").is_ok());
    let err = verdict("row 7 missing from index").unwrap_err();
    assert!(
        err.to_string().contains("row 7 missing from index"),
        "{err}"
    );
}

#[test]
fn a_database_at_this_schema_is_accepted_as_it_is() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("puddle.db");
    open(&path);
    let staged = check_staged_database(&path).unwrap().unwrap();
    assert_eq!(staged.schema_version, SCHEMA_VERSION);
}

#[test]
fn migrating_a_database_brings_it_to_this_schema_and_refuses_a_newer_one() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("puddle.db");
    {
        let mut conn = Connection::open(&path).unwrap();
        schema::migrate_up_to(&mut conn, 20).unwrap();
    }
    migrate_database(&path).unwrap();
    assert_eq!(
        count(&path, "PRAGMA user_version"),
        i64::from(SCHEMA_VERSION)
    );
    sql(
        &path,
        &format!("PRAGMA user_version = {};", SCHEMA_VERSION + 1),
    );
    assert!(migrate_database(&path).is_err());
}

#[test]
fn a_restore_migrates_an_old_present_database_before_it_copies_it() {
    let dir = tempfile::tempdir().unwrap();
    let here = dir.path().join("here.db");
    let there = dir.path().join("there.db");
    {
        let mut conn = Connection::open(&here).unwrap();
        schema::migrate_up_to(&mut conn, 20).unwrap();
    }
    open(&there);
    let dest = dir.path().join("new.db");
    restore_database(&here, &there, &dest, RestoreMode::Add).unwrap();
    assert_eq!(
        count(&dest, "PRAGMA user_version"),
        i64::from(SCHEMA_VERSION)
    );
    assert_eq!(
        count(&here, "PRAGMA user_version"),
        i64::from(SCHEMA_VERSION)
    );
}

#[test]
fn a_rule_that_came_from_a_pending_request_does_not_point_at_one_that_is_not_restored() {
    let dir = tempfile::tempdir().unwrap();
    let (here, there) = pair(dir.path());
    sql(
        &there,
        "UPDATE rules SET source_pending_id = (SELECT id FROM pending LIMIT 1) WHERE pattern = 'new.example.org';",
    );
    assert_eq!(
        count(
            &there,
            "SELECT count(*) FROM rules WHERE source_pending_id IS NOT NULL"
        ),
        1
    );
    let dest = dir.path().join("new.db");
    restore_database(&here, &there, &dest, RestoreMode::Add).unwrap();
    assert_eq!(
        count(
            &dest,
            "SELECT count(*) FROM rules WHERE pattern = 'new.example.org' AND source_pending_id IS NULL"
        ),
        1
    );
}

#[test]
fn ids_of_the_backup_are_translated_when_this_machine_numbers_its_rows_differently() {
    let dir = tempfile::tempdir().unwrap();
    let (here, there) = pair(dir.path());
    // Rows here that take the ids the backup's users, sets and variables have there.
    sql(
        &here,
        "INSERT INTO users (role, name, created_at) VALUES ('user', 'Zed', 1);
         INSERT INTO rule_sets (name, description, created_at, created_by, user_id) VALUES ('Other', '', 1, 'cli', 1);
         INSERT INTO env_vars (workspace_id, name, kind, value, changed_at, user_id)
             VALUES ('w9', 'OTHER', 'plain', 'v', 1, 1);",
    );
    sql(
        &there,
        "INSERT INTO env_vars (workspace_id, name, kind, value, changed_at, user_id)
             VALUES ('w1', 'PLAIN', 'plain', 'v', 1, 1);
         INSERT INTO env_stand_ins (workspace_id, env_id, stand_in)
             VALUES ('w1', (SELECT id FROM env_vars WHERE name = 'PLAIN'), 'standin');",
    );
    let dest = dir.path().join("new.db");
    restore_database(&here, &there, &dest, RestoreMode::Add).unwrap();
    assert_eq!(
        count(
            &dest,
            "SELECT count(*) FROM rules r JOIN users u ON u.id = r.user_id \
             WHERE r.pattern = 'ann.example.org' AND u.name = 'Ann'"
        ),
        1,
        "a rule of Ann follows Ann to her new id"
    );
    assert_eq!(
        count(
            &dest,
            "SELECT count(*) FROM rules r JOIN rule_sets s ON s.id = r.set_id \
             WHERE r.pattern = 'in-set.example.org' AND s.name = 'Mine'"
        ),
        1,
        "a rule of a set follows the set to its new id"
    );
    assert_eq!(
        count(
            &dest,
            "SELECT count(*) FROM env_stand_ins s JOIN env_vars e ON e.id = s.env_id \
             WHERE e.name = 'PLAIN' AND s.stand_in = 'standin'"
        ),
        1,
        "a stand-in follows its variable to its new id"
    );
}

#[test]
fn the_audit_log_rows_get_new_ids_in_the_order_of_the_backup() {
    let dir = tempfile::tempdir().unwrap();
    let (here, there) = pair(dir.path());
    sql(
        &there,
        "INSERT INTO audit (id, ts, type, line, user_id) VALUES (40, 1, 'x', '{}', 1);
         INSERT INTO audit (id, ts, type, line, user_id) VALUES (30, 2, 'y', '{}', 1);",
    );
    let dest = dir.path().join("new.db");
    restore_database(&here, &there, &dest, RestoreMode::Add).unwrap();
    let conn = Connection::open(&dest).unwrap();
    let mut stmt = conn
        .prepare("SELECT id || type FROM audit ORDER BY id")
        .unwrap();
    let rows: Vec<String> = stmt
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(rows, ["1y", "2x"]);
}

#[test]
fn the_preview_lists_the_pasted_tokens_of_the_backups_identities_and_no_other_source() {
    use crate::identity::{Coverage, CredentialBinding};
    use puddle_types::{HostName, SourceSpec, StoredId, TokenScope};
    let host = HostName::new("github.com").unwrap();
    let rest = Coverage::new(std::collections::BTreeSet::new(), true).unwrap();
    let pasted = CredentialBinding::new(
        &host,
        SourceSpec::Stored {
            id: StoredId::new("tok-1").unwrap(),
            scope: TokenScope {
                host: host.clone(),
                org: None,
            },
        },
        rest.clone(),
    )
    .unwrap();
    let gh = CredentialBinding::new(
        &host,
        SourceSpec::Gh {
            host: host.clone(),
            account: puddle_types::AccountName::new("octo").unwrap(),
        },
        rest,
    )
    .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (here, there) = (dir.path().join("here.db"), dir.path().join("there.db"));
    open(&here);
    open(&there);
    let insert = |label: &str, bindings: &[CredentialBinding]| {
        format!(
            "INSERT INTO identities (user_id, label, author_name, author_email, credentials, position, is_default, created_at, changed_at) \
             VALUES (1, '{label}', 'A', 'a@example.org', '{}', 0, 0, 1, 1);",
            serde_json::to_string(bindings).unwrap()
        )
    };
    sql(
        &there,
        &[
            insert("work", &[gh.clone(), pasted.clone()]),
            insert("plain", &[gh]),
            // Not the shape this build writes: no token is claimed, and nothing fails.
            "INSERT INTO identities (user_id, label, author_name, author_email, credentials, position, is_default, created_at, changed_at) \
             VALUES (1, 'odd', 'A', 'a@example.org', 'not json', 1, 0, 1, 1);"
                .to_owned(),
        ]
        .concat(),
    );
    let preview = preview_merge(&here, &there).unwrap();
    assert_eq!(
        preview.identity_tokens,
        vec![IdentityToken {
            identity: "work".into(),
            host: "github.com".into(),
            secret_id: "tok-1".into()
        }]
    );
}
