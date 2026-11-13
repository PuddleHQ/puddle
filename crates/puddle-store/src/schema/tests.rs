// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

fn version(conn: &Connection) -> u32 {
    conn.pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap()
}

#[test]
fn fresh_database_migrates_to_latest() {
    let mut conn = Connection::open_in_memory().unwrap();
    assert_eq!(migrate(&mut conn).unwrap(), 0);
    assert_eq!(version(&conn), SCHEMA_VERSION);
    assert_eq!(MIGRATIONS.len(), SCHEMA_VERSION as usize);
}

#[test]
fn v3_keeps_every_rule_and_never_reuses_a_deleted_id() {
    let mut conn = Connection::open_in_memory().unwrap();
    migrate_up_to(&mut conn, 2).unwrap();
    conn.execute_batch(
            "INSERT INTO rules (scope, sandbox_id, pattern_kind, pattern, effect, created_at, created_by)
                VALUES ('global', NULL, 'exact', 'a.example', 'allow', 1, 'cli'),
                       ('sandbox', 'box', 'suffix', '.example.com', 'deny', 2, 'ui'),
                       ('global', NULL, 'exact', 'gone.example', 'allow', 3, 'api');
             DELETE FROM rules WHERE pattern = 'gone.example';
             INSERT INTO pending (sandbox_id, host, port, first_seen, last_seen)
                VALUES ('box', 'x.example', 443, 1, 1);",
        )
        .unwrap();
    conn.pragma_update(None, "foreign_keys", true).unwrap();
    migrate(&mut conn).unwrap();
    let kept: Vec<(i64, String, Option<String>, Option<i64>)> = conn
        .prepare("SELECT id, scope, workspace_id, set_id FROM rules ORDER BY id")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        kept,
        vec![
            (1, "global".into(), None, None),
            (2, "workspace".into(), Some("box".into()), None)
        ]
    );
    conn.execute(
        "INSERT INTO rules (scope, pattern_kind, pattern, effect, created_at, created_by, user_id)
             VALUES ('global', 'exact', 'new.example', 'allow', 4, 'cli', 1)",
        [],
    )
    .unwrap();
    assert_eq!(
        conn.last_insert_rowid(),
        4,
        "id 3 was deleted before and stays unused"
    );
    // A set entry needs its set, and a set scope needs a set id.
    assert!(
            conn.execute(
                "INSERT INTO rules (scope, set_id, pattern_kind, pattern, effect, created_at, created_by, user_id)
                 VALUES ('set', 9, 'exact', 'a.example', 'allow', 4, 'cli', 1)",
                [],
            )
            .is_err()
        );
    assert!(
        conn.execute(
            "INSERT INTO rules (scope, pattern_kind, pattern, effect, created_at, created_by, user_id)
                 VALUES ('set', 'exact', 'a.example', 'allow', 4, 'cli', 1)",
            [],
        )
        .is_err()
    );
    let rule_set: Option<String> = conn
        .query_row("SELECT rule_set FROM pending", [], |r| r.get(0))
        .unwrap();
    assert_eq!(rule_set, None);
}

/// A version 3 database with a row in every table that names the owner, migrated to the latest.
fn migrated_from_the_rename() -> Connection {
    let mut conn = Connection::open_in_memory().unwrap();
    migrate_up_to(&mut conn, 3).unwrap();
    conn.execute_batch(
            "INSERT INTO rules (scope, sandbox_id, pattern_kind, pattern, effect, created_at, created_by)
                VALUES ('global', NULL, 'exact', 'a.example', 'allow', 1, 'cli'),
                       ('sandbox', 'box', 'suffix', '.example.com', 'deny', 2, 'ui'),
                       ('global', NULL, 'exact', 'gone.example', 'allow', 3, 'api');
             DELETE FROM rules WHERE pattern = 'gone.example';
             INSERT INTO rule_sets (name, created_at, created_by) VALUES ('mine', 1, 'ui');
             INSERT INTO rules (scope, set_id, pattern_kind, pattern, effect, created_at, created_by)
                VALUES ('set', 1, 'exact', 'set.example', 'allow', 4, 'ui');
             INSERT INTO pending (sandbox_id, host, port, first_seen, last_seen)
                VALUES ('box', 'x.example', 443, 1, 1);
             INSERT INTO audit (ts, type, sandbox_id, line)
                VALUES (5, 'connection', 'box', '{\"type\":\"connection\",\"sandbox_id\":\"box\"}');
             INSERT INTO rule_set_switches (rule_set, sandbox_id, enabled, changed_at)
                VALUES ('builtin:x', NULL, 1, 1), ('builtin:x', 'box', 0, 2);
             INSERT INTO system_reasons (sandbox_id, reason) VALUES ('box', 'why');",
        )
        .unwrap();
    conn.pragma_update(None, "foreign_keys", true).unwrap();
    assert_eq!(migrate(&mut conn).unwrap(), 3);
    assert_eq!(version(&conn), SCHEMA_VERSION);
    conn
}

/// A database a Puddle from before the rename left behind: rows in every table that names the
/// owner, then migrated.
/// A set written before sets had a colour reads back with none stored, and a stored colour
/// that is not readable is the same as none.
#[test]
fn v13_adds_the_colour_column_and_old_sets_have_none() {
    let mut conn = Connection::open_in_memory().unwrap();
    migrate_up_to(&mut conn, 12).unwrap();
    conn.execute_batch(
        "INSERT INTO rule_sets (name, created_at, created_by) VALUES ('old', 1, 'ui');",
    )
    .unwrap();
    assert_eq!(migrate(&mut conn).unwrap(), 12);
    assert_eq!(version(&conn), SCHEMA_VERSION);
    let fill: Option<String> = conn
        .query_row("SELECT fill FROM rule_sets WHERE name = 'old'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(fill, None);
}

#[test]
fn v4_renames_the_owner_everywhere_and_keeps_the_rows() {
    let conn = migrated_from_the_rename();
    let rules: Vec<(i64, String, Option<String>)> = conn
        .prepare("SELECT id, scope, workspace_id FROM rules ORDER BY id")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        rules,
        vec![
            (1, "global".into(), None),
            (2, "workspace".into(), Some("box".into())),
            (4, "set".into(), None),
        ]
    );
    let count = |sql: &str| -> i64 { conn.query_row(sql, [], |r| r.get(0)).unwrap() };
    assert_eq!(
        count("SELECT count(*) FROM pending WHERE workspace_id = 'box'"),
        1
    );
    assert_eq!(
        count("SELECT count(*) FROM audit WHERE workspace_id = 'box'"),
        1
    );
    assert_eq!(
        count("SELECT count(*) FROM rule_set_switches WHERE workspace_id = 'box'"),
        1
    );
    assert_eq!(
        count("SELECT count(*) FROM rule_set_switches WHERE workspace_id IS NULL"),
        1
    );
    assert_eq!(
        count("SELECT count(*) FROM system_reasons WHERE workspace_id = 'box'"),
        1
    );
    // The audit line is kept as it was written.
    let line: String = conn
        .query_row("SELECT line FROM audit", [], |r| r.get(0))
        .unwrap();
    assert!(line.contains("sandbox_id"));
    // Nothing of the old name is left in the schema, apart from the stored lines.
    assert_eq!(
        count("SELECT count(*) FROM sqlite_schema WHERE sql LIKE '%sandbox%'"),
        0
    );
}

#[test]
fn v4_keeps_the_guards_of_the_rules_and_pending_tables() {
    let conn = migrated_from_the_rename();
    // A rule's owner and scope agree, a deleted rule's id is not reused, a pending row's
    // owner never changes, and the one-open-row key still applies.
    assert!(
        conn.execute(
            "INSERT INTO rules (scope, pattern_kind, pattern, effect, created_at, created_by, user_id)
                 VALUES ('workspace', 'exact', 'a.example', 'allow', 4, 'cli', 1)",
            [],
        )
        .is_err()
    );
    assert!(
            conn.execute(
                "INSERT INTO rules (scope, workspace_id, pattern_kind, pattern, effect, created_at, created_by, user_id)
                 VALUES ('sandbox', 'box', 'exact', 'a.example', 'allow', 4, 'cli', 1)",
                [],
            )
            .is_err()
        );
    conn.execute(
        "INSERT INTO rules (scope, pattern_kind, pattern, effect, created_at, created_by, user_id)
             VALUES ('global', 'exact', 'new.example', 'allow', 5, 'cli', 1)",
        [],
    )
    .unwrap();
    assert_eq!(conn.last_insert_rowid(), 5, "ids 3 and 4 stay used");
    assert!(
        conn.execute("UPDATE pending SET workspace_id = 'other'", [])
            .is_err()
    );
    assert!(
        conn.execute(
            "INSERT INTO pending (workspace_id, host, port, first_seen, last_seen)
                 VALUES ('box', 'x.example', 443, 1, 1)",
            [],
        )
        .is_err()
    );
}

/// A database from before identities: every row stays, the new tables start empty, and the
/// switches' defaults apply to a workspace that has no row.
#[test]
fn v5_adds_the_identity_tables_and_keeps_every_older_row() {
    let mut conn = Connection::open_in_memory().unwrap();
    migrate_up_to(&mut conn, 4).unwrap();
    conn.execute_batch(
        "INSERT INTO rules (scope, pattern_kind, pattern, effect, created_at, created_by)
                VALUES ('global', 'exact', 'a.example', 'allow', 1, 'cli');
             INSERT INTO pending (workspace_id, host, port, first_seen, last_seen)
                VALUES ('box', 'x.example', 443, 1, 1);",
    )
    .unwrap();
    conn.pragma_update(None, "foreign_keys", true).unwrap();
    assert_eq!(migrate(&mut conn).unwrap(), 4);
    assert_eq!(version(&conn), SCHEMA_VERSION);
    let count = |sql: &str| -> i64 { conn.query_row(sql, [], |r| r.get(0)).unwrap() };
    assert_eq!(count("SELECT count(*) FROM rules"), 1);
    assert_eq!(count("SELECT count(*) FROM pending"), 1);
    for table in [
        "identities",
        "workspace_identities",
        "workspace_git",
        "workspace_repos",
    ] {
        assert_eq!(
            count(&format!("SELECT count(*) FROM {table}")),
            0,
            "{table}"
        );
    }
    conn.execute(
        "INSERT INTO workspace_git (workspace_id) VALUES ('box')",
        [],
    )
    .unwrap();
    let switches: (Option<i64>, Option<i64>) = conn
        .query_row(
            "SELECT only_push_listed, only_pull_listed FROM workspace_git",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(switches, (None, None), "a new row follows the defaults");
    // One default at most, and a workspace's order has no gaps in its key.
    let add = "INSERT INTO identities (label, author_name, author_email, credentials, position, is_default, created_at, changed_at, user_id)
                   VALUES (?1, 'n', 'e@x', '[]', ?2, 1, 1, 1, 1)";
    conn.execute(add, ("a", 0)).unwrap();
    assert!(conn.execute(add, ("b", 1)).is_err());
}

/// A database from before environment variables: every row stays, the new tables start empty,
/// and the table's own checks keep a secret's value out of it.
#[test]
fn v6_adds_the_environment_tables_and_refuses_a_row_of_the_wrong_shape() {
    let mut conn = Connection::open_in_memory().unwrap();
    migrate_up_to(&mut conn, 5).unwrap();
    conn.execute_batch(
            "INSERT INTO identities (label, author_name, author_email, credentials, position, is_default, created_at, changed_at)
                VALUES ('a', 'n', 'e@x', '[]', 0, 1, 1, 1);",
        )
        .unwrap();
    conn.pragma_update(None, "foreign_keys", true).unwrap();
    assert_eq!(migrate(&mut conn).unwrap(), 5);
    assert_eq!(version(&conn), SCHEMA_VERSION);
    let count = |sql: &str| -> i64 { conn.query_row(sql, [], |r| r.get(0)).unwrap() };
    assert_eq!(count("SELECT count(*) FROM identities"), 1);
    assert_eq!(count("SELECT count(*) FROM env_vars"), 0);
    assert_eq!(count("SELECT count(*) FROM env_stand_ins"), 0);
    let insert = |kind: &str, value: Option<&str>, secret: Option<&str>, hosts: Option<&str>| {
        conn.execute(
            "INSERT INTO env_vars (workspace_id, name, kind, value, secret_id, hosts, changed_at, user_id)
                 VALUES (NULL, 'A', ?1, ?2, ?3, ?4, 1, 1)",
            (kind, value, secret, hosts),
        )
    };
    // A plain variable has a value and nothing of a secret; a secret has its id and hosts and no value.
    assert!(insert("plain", None, None, None).is_err());
    assert!(insert("plain", Some("v"), Some("id"), None).is_err());
    assert!(insert("secret", Some("v"), Some("id"), Some("[]")).is_err());
    assert!(insert("secret", None, None, Some("[]")).is_err());
    assert!(insert("secret", None, Some("id"), None).is_err());
    assert!(insert("other", Some("v"), None, None).is_err());
    insert("secret", None, Some("id"), Some("[]")).unwrap();
    // One name per scope, in both the global and a workspace's scope.
    assert!(insert("plain", Some("v"), None, None).is_err());
    conn.execute(
            "INSERT INTO env_vars (workspace_id, name, kind, value, changed_at, user_id) VALUES ('box', 'A', 'plain', 'v', 1, 1)",
            [],
        )
        .unwrap();
    assert!(
            conn.execute(
                "INSERT INTO env_vars (workspace_id, name, kind, value, changed_at, user_id) VALUES ('box', 'A', 'plain', 'w', 1, 1)",
                [],
            )
            .is_err()
        );
    // A stand-in goes with its variable.
    conn.execute(
            "INSERT INTO env_stand_ins (workspace_id, env_id, stand_in) VALUES ('box', 1, 'puddle-secret-A-0')",
            [],
        )
        .unwrap();
    conn.execute("DELETE FROM env_vars WHERE id = 1", [])
        .unwrap();
    assert_eq!(count("SELECT count(*) FROM env_stand_ins"), 0);
}

/// A secret stored before places existed keeps its row and is swapped in header values (no
/// place); only a secret can have a place.
#[test]
fn v13_gives_existing_secrets_no_place_and_a_plain_variable_none() {
    let mut conn = Connection::open_in_memory().unwrap();
    migrate_up_to(&mut conn, 12).unwrap();
    conn.execute_batch(
        "INSERT INTO env_vars (workspace_id, name, kind, value, secret_id, hosts, changed_at)
                VALUES (NULL, 'S', 'secret', NULL, 'id', '[\"a.example.com\"]', 1);
             INSERT INTO env_vars (workspace_id, name, kind, value, changed_at)
                VALUES (NULL, 'P', 'plain', 'v', 1);",
    )
    .unwrap();
    assert_eq!(migrate(&mut conn).unwrap(), 12);
    let place = |name: &str| -> Option<String> {
        conn.query_row("SELECT place FROM env_vars WHERE name = ?1", [name], |r| {
            r.get(0)
        })
        .unwrap()
    };
    assert_eq!((place("S"), place("P")), (None, None));
    conn.execute("UPDATE env_vars SET place = 'path' WHERE name = 'S'", [])
        .unwrap();
    assert!(
        conn.execute("UPDATE env_vars SET place = 'path' WHERE name = 'P'", [])
            .is_err()
    );
}

/// A workspace that set its switches before the defaults existed keeps them as explicit
/// values, and the defaults table starts empty (so the built-in defaults apply).
#[test]
fn v7_keeps_switches_written_before_it_as_explicit_values() {
    let mut conn = Connection::open_in_memory().unwrap();
    migrate_up_to(&mut conn, 6).unwrap();
    conn.execute_batch(
        "INSERT INTO workspace_git VALUES ('box', 0, 1);
             INSERT INTO workspace_git VALUES ('other', 1, 0);",
    )
    .unwrap();
    assert_eq!(migrate(&mut conn).unwrap(), 6);
    let read = |ws: &str| -> (Option<i64>, Option<i64>) {
        conn.query_row(
            "SELECT only_push_listed, only_pull_listed FROM workspace_git WHERE workspace_id = ?1",
            [ws],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
    };
    assert_eq!(read("box"), (Some(0), Some(1)));
    assert_eq!(read("other"), (Some(1), Some(0)));
    let defaults: i64 = conn
        .query_row("SELECT count(*) FROM git_defaults", [], |r| r.get(0))
        .unwrap();
    assert_eq!(defaults, 0);
    // The defaults table holds one row per user at most.
    let add = "INSERT INTO git_defaults VALUES (?1, 1, 0)";
    conn.execute(add, [1]).unwrap();
    assert!(conn.execute(add, [1]).is_err());
}

/// A database from before pinned forwards: every row stays, the new table starts empty, and the
/// table's own checks keep a port inside what a port can be.
#[test]
fn v8_adds_the_pinned_ports_table_and_refuses_a_port_outside_the_range() {
    let mut conn = Connection::open_in_memory().unwrap();
    migrate_up_to(&mut conn, 7).unwrap();
    conn.execute_batch("INSERT INTO git_defaults VALUES (1, 1, 0);")
        .unwrap();
    assert_eq!(migrate(&mut conn).unwrap(), 7);
    assert_eq!(version(&conn), SCHEMA_VERSION);
    let count = |sql: &str| -> i64 { conn.query_row(sql, [], |r| r.get(0)).unwrap() };
    assert_eq!(count("SELECT count(*) FROM git_defaults"), 1);
    assert_eq!(count("SELECT count(*) FROM pinned_ports"), 0);
    let add = "INSERT INTO pinned_ports VALUES (?1, ?2, ?3)";
    conn.execute(add, ("box", 3000, 3000)).unwrap();
    for (guest, host) in [(0, 3000), (3000, 0), (65_536, 80), (80, 65_536)] {
        assert!(
            conn.execute(add, ("box", guest, host)).is_err(),
            "{guest} {host}"
        );
    }
    assert!(
        conn.execute(add, ("box", 3000, 3001)).is_err(),
        "one pin per guest port"
    );
}

#[test]
fn migrating_twice_is_a_no_op() {
    let mut conn = Connection::open_in_memory().unwrap();
    migrate(&mut conn).unwrap();
    assert_eq!(migrate(&mut conn).unwrap(), SCHEMA_VERSION);
    assert_eq!(version(&conn), SCHEMA_VERSION);
}

#[test]
fn newer_schema_is_refused() {
    let mut conn = Connection::open_in_memory().unwrap();
    conn.pragma_update(None, "user_version", SCHEMA_VERSION + 1)
        .unwrap();
    assert!(matches!(
        migrate(&mut conn),
        Err(StoreError::SchemaTooNew { found, supported })
            if found == SCHEMA_VERSION + 1 && supported == SCHEMA_VERSION
    ));
}

#[test]
fn later_migrations_apply_in_order_and_failures_roll_back() {
    let mut conn = Connection::open_in_memory().unwrap();
    let steps = [
        "CREATE TABLE a (x INTEGER);",
        "ALTER TABLE a ADD COLUMN y INTEGER;",
    ];
    migrate_with(&mut conn, &steps[..1]).unwrap();
    assert_eq!(version(&conn), 1);
    migrate_with(&mut conn, &steps).unwrap();
    assert_eq!(version(&conn), 2);
    let broken = [steps[0], steps[1], "CREATE TABLE b (x INTEGER); NOT SQL;"];
    assert!(migrate_with(&mut conn, &broken).is_err());
    assert_eq!(version(&conn), 2);
    let b_exists: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_schema WHERE name = 'b'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(b_exists, 0);
}

/// A rule made by a workspace create is stored; every older rule keeps its row, its id and
/// the `AUTOINCREMENT` high-water mark, and the guards of the rebuilt table still hold.
#[test]
fn v17_lets_a_workspace_create_make_a_rule_and_keeps_the_older_rules() {
    let mut conn = Connection::open_in_memory().unwrap();
    migrate_up_to(&mut conn, 16).unwrap();
    conn.execute_batch(
            "INSERT INTO rules (scope, pattern_kind, pattern, effect, created_at, created_by)
                VALUES ('global', 'exact', 'a.example', 'allow', 1, 'cli');
             INSERT INTO rules (scope, workspace_id, pattern_kind, pattern, effect, created_at, created_by)
                VALUES ('workspace', 'box', 'exact', 'b.example', 'deny', 2, 'ui');
             DELETE FROM rules WHERE id = 2;",
        )
        .unwrap();
    assert!(
        conn.execute(
            "INSERT INTO rules (scope, pattern_kind, pattern, effect, created_at, created_by)
                 VALUES ('global', 'exact', 'c.example', 'allow', 3, 'workspace_create')",
            [],
        )
        .is_err(),
        "version 16 does not know the actor"
    );
    assert_eq!(migrate(&mut conn).unwrap(), 16);
    assert_eq!(version(&conn), SCHEMA_VERSION);
    let survivors: Vec<(i64, String)> = conn
        .prepare("SELECT id, pattern FROM rules")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(survivors, [(1, "a.example".to_owned())]);
    conn.execute(
            "INSERT INTO rules (scope, workspace_id, pattern_kind, pattern, effect, created_at, created_by, user_id)
             VALUES ('workspace', 'box', 'exact', 'github.com', 'allow', 3, 'workspace_create', 1)",
            [],
        )
        .unwrap();
    assert_eq!(conn.last_insert_rowid(), 3, "the deleted id 2 stays used");
    for bad in [
        "INSERT INTO rules (scope, pattern_kind, pattern, effect, created_at, created_by, user_id)
             VALUES ('global', 'exact', 'x.example', 'allow', 4, 'system', 1)",
        "INSERT INTO rules (scope, pattern_kind, pattern, effect, created_at, created_by, user_id)
             VALUES ('workspace', 'exact', 'x.example', 'allow', 4, 'cli', 1)",
    ] {
        assert!(conn.execute(bad, []).is_err(), "{bad}");
    }
}

/// A database from before the three modes: a workspace whose switch was on keeps decrypting
/// every host, one whose switch was off stays off, and no global mode is set, so the install
/// behaves as it did until the user chooses.
#[test]
fn v18_keeps_what_each_old_switch_did_and_leaves_the_global_mode_unchosen() {
    let mut conn = Connection::open_in_memory().unwrap();
    migrate_up_to(&mut conn, 17).unwrap();
    conn.execute_batch(
            "INSERT INTO workspace_interception (workspace_id, decrypt_all) VALUES ('on', 1), ('off', 0);
             INSERT INTO never_decrypt (workspace_id, pattern) VALUES ('on', 'bank.example');",
        )
        .unwrap();
    assert_eq!(migrate(&mut conn).unwrap(), 17);
    let mode = |name: &str| -> Option<String> {
        conn.query_row(
            "SELECT mode FROM workspace_interception WHERE workspace_id = ?1",
            [name],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(mode("on").as_deref(), Some("all"));
    assert_eq!(mode("off").as_deref(), Some("known_only"));
    let globals: i64 = conn
        .query_row("SELECT count(*) FROM interception_settings", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(globals, 0, "no global mode until the user chooses");
    let kept: i64 = conn
        .query_row("SELECT count(*) FROM never_decrypt", [], |r| r.get(0))
        .unwrap();
    assert_eq!(kept, 1);
    assert!(
        conn.execute(
            "INSERT INTO workspace_interception (workspace_id, mode) VALUES ('x', 'most')",
            []
        )
        .is_err(),
        "the table refuses a mode it does not know"
    );
}

/// Version 19 adds the history of refused hosts to a database of version 18, which has none.
#[test]
fn v19_adds_an_empty_history_of_refused_hosts_and_keeps_the_rest() {
    let mut conn = Connection::open_in_memory().unwrap();
    migrate_up_to(&mut conn, 18).unwrap();
    conn.execute_batch(
        "INSERT INTO workspace_interception (workspace_id, mode) VALUES ('a', 'all');",
    )
    .unwrap();
    assert_eq!(migrate(&mut conn).unwrap(), 18);
    assert_eq!(version(&conn), SCHEMA_VERSION);
    let kept: i64 = conn
        .query_row("SELECT count(*) FROM workspace_interception", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(kept, 1);
    let insert = |host: &str, count: i64| {
        conn.execute(
            "INSERT INTO learned_hosts (workspace_id, host, first_at, last_at, count)
                 VALUES ('a', ?1, 1, 2, ?2)",
            rusqlite::params![host, count],
        )
    };
    insert("x.example", 1).unwrap();
    assert!(
        insert("x.example", 1).is_err(),
        "one row per workspace and host"
    );
    assert!(
        insert("y.example", 0).is_err(),
        "a refused host was refused at least once"
    );
}

/// A database from before users: the owner is user 1 and owns every row, each workspace keeps its
/// identity list through the identities rebuild, names become unique per user, and the owner and
/// a workspace's user are fixed.
#[test]
fn v20_gives_everything_to_the_owner_and_keeps_every_row() {
    let mut conn = Connection::open_in_memory().unwrap();
    conn.pragma_update(None, "foreign_keys", true).unwrap();
    migrate_up_to(&mut conn, 19).unwrap();
    conn.execute_batch(
        "INSERT INTO identities (label, author_name, author_email, credentials, position, is_default, created_at, changed_at)
             VALUES ('work', 'n', 'e@x', '[]', 0, 1, 1, 1), ('home', 'n', 'h@x', '[]', 1, 0, 1, 1);
         DELETE FROM identities WHERE label = 'home';
         INSERT INTO identities (label, author_name, author_email, credentials, position, is_default, created_at, changed_at)
             VALUES ('home', 'n', 'h@x', '[]', 1, 0, 1, 1);
         INSERT INTO workspace_identities VALUES ('box', 1, 0), ('box', 3, 1);
         INSERT INTO git_defaults VALUES (1, 0, 1);
         INSERT INTO rule_sets (name, created_at, created_by) VALUES ('Mine', 1, 'ui');
         INSERT INTO rules (scope, pattern_kind, pattern, effect, created_at, created_by)
             VALUES ('global', 'exact', 'a.example', 'allow', 1, 'ui');
         INSERT INTO env_vars (workspace_id, name, kind, value, changed_at) VALUES (NULL, 'A', 'plain', 'v', 1);",
    )
    .unwrap();
    assert_eq!(migrate(&mut conn).unwrap(), 19);
    assert_eq!(version(&conn), SCHEMA_VERSION);
    let one = |sql: &str| -> i64 { conn.query_row(sql, [], |r| r.get(0)).unwrap() };
    assert_eq!(
        one("SELECT count(*) FROM users WHERE id = 1 AND role = 'owner'"),
        1
    );
    for table in ["rules", "rule_sets", "env_vars", "identities"] {
        assert_eq!(
            one(&format!("SELECT count(*) FROM {table} WHERE user_id <> 1")),
            0,
            "{table}"
        );
    }
    assert_eq!(one("SELECT count(*) FROM identities"), 2);
    let kept: String = conn
        .query_row(
            "SELECT group_concat(identity_id, ',') FROM
                 (SELECT identity_id FROM workspace_identities ORDER BY position)",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(kept, "1,3", "the workspace keeps its identities in order");
    assert_eq!(
        one("SELECT only_pull_listed FROM git_defaults WHERE user_id = 1"),
        1
    );
    // A deleted identity's id is still never reused.
    conn.execute(
        "INSERT INTO identities (label, author_name, author_email, credentials, position, created_at, changed_at, user_id)
             VALUES ('new', 'n', 'e@x', '[]', 2, 1, 1, 1)",
        [],
    )
    .unwrap();
    assert_eq!(conn.last_insert_rowid(), 4);
    // Names and the one default are unique per user, not per install.
    conn.execute(
        "INSERT INTO users (role, name, created_at) VALUES ('user', 'ann', 1)",
        [],
    )
    .unwrap();
    let identity = "INSERT INTO identities (user_id, label, author_name, author_email, credentials, position, is_default, created_at, changed_at)
                        VALUES (?1, 'WORK', 'n', 'e@x', '[]', 0, 1, 1, 1)";
    assert!(conn.execute(identity, [1]).is_err());
    conn.execute(identity, [2]).unwrap();
    let set = "INSERT INTO rule_sets (user_id, name, created_at, created_by) VALUES (?1, 'mine', 1, 'ui')";
    assert!(conn.execute(set, [1]).is_err());
    conn.execute(set, [2]).unwrap();
    let env = "INSERT INTO env_vars (user_id, workspace_id, name, kind, value, changed_at) VALUES (?1, NULL, 'A', 'plain', 'v', 1)";
    assert!(conn.execute(env, [1]).is_err());
    conn.execute(env, [2]).unwrap();
    // The owner stays, one owner only, and a workspace's user never changes.
    assert!(conn.execute("DELETE FROM users WHERE id = 1", []).is_err());
    assert!(
        conn.execute("UPDATE users SET role = 'owner' WHERE id = 2", [])
            .is_err()
    );
    assert!(
        conn.execute(
            "INSERT INTO users (role, name, created_at) VALUES ('owner', 'x', 1)",
            []
        )
        .is_err()
    );
    conn.execute("INSERT INTO workspace_owners VALUES ('box', 2)", [])
        .unwrap();
    assert!(
        conn.execute("UPDATE workspace_owners SET user_id = 1", [])
            .is_err()
    );
    assert!(
        conn.execute("INSERT INTO workspace_owners VALUES ('other', 9)", [])
            .is_err()
    );
}

/// V24 rebuilds the seven tables that carried `DEFAULT 1` on `user_id`: every row, every id, the
/// `AUTOINCREMENT` high-water mark, the indexes, the audit size triggers and the identity lists
/// survive, and an insert that forgets the user now fails instead of writing the owner's row.
#[test]
fn v24_drops_the_owner_default_and_keeps_every_row() {
    let mut conn = Connection::open_in_memory().unwrap();
    conn.pragma_update(None, "foreign_keys", true).unwrap();
    migrate_up_to(&mut conn, 23).unwrap();
    conn.execute_batch(
        "INSERT INTO users (role, name, created_at) VALUES ('user', 'ann', 1);
         INSERT INTO rule_sets (name, created_at, created_by, user_id) VALUES ('Mine', 1, 'ui', 2);
         INSERT INTO rules (scope, set_id, pattern_kind, pattern, effect, created_at, created_by, user_id)
             VALUES ('set', 1, 'exact', 'set.example', 'allow', 1, 'ui', 2);
         INSERT INTO rules (scope, pattern_kind, pattern, effect, created_at, created_by, user_id)
             VALUES ('global', 'exact', 'a.example', 'allow', 1, 'ui', 1),
                    ('global', 'exact', 'gone.example', 'allow', 1, 'ui', 1);
         DELETE FROM rules WHERE pattern = 'gone.example';
         INSERT INTO request_rules (scope, host, methods, path, created_at, created_by, user_id)
             VALUES ('global', 'h.example', '*', '/*', 1, 'api', 2);
         INSERT INTO audit (ts, type, workspace_id, line, user_id) VALUES (1, 'x', 'box', 'abcd', 2);
         INSERT INTO rule_set_switches (rule_set, workspace_id, enabled, changed_at, user_id)
             VALUES ('1', 'box', 1, 1, 2);
         INSERT INTO env_vars (workspace_id, name, kind, value, changed_at, user_id)
             VALUES (NULL, 'A', 'plain', 'v', 1, 2);
         INSERT INTO identities (user_id, label, author_name, author_email, credentials, position,
                                 is_default, created_at, changed_at)
             VALUES (2, 'work', 'n', 'e@x', '[]', 0, 1, 1, 1);
         INSERT INTO workspace_identities VALUES ('box', 1, 0);",
    )
    .unwrap();
    let size_before: i64 = conn
        .query_row("SELECT bytes FROM audit_size WHERE id = 1", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(migrate(&mut conn).unwrap(), 23);
    assert_eq!(version(&conn), SCHEMA_VERSION);
    let one = |sql: &str| -> i64 { conn.query_row(sql, [], |r| r.get(0)).unwrap() };
    for (table, rows) in [
        ("rules", 2),
        ("request_rules", 1),
        ("audit", 1),
        ("rule_sets", 1),
        ("rule_set_switches", 1),
        ("env_vars", 1),
        ("identities", 1),
    ] {
        assert_eq!(
            one(&format!("SELECT count(*) FROM {table}")),
            rows,
            "{table}"
        );
        let default: Option<String> = conn
            .query_row(
                "SELECT dflt_value FROM pragma_table_info(?1) WHERE name = 'user_id'",
                [table],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(default, None, "{table} still defaults its user");
    }
    assert_eq!(
        one("SELECT user_id FROM rules WHERE pattern = 'set.example'"),
        2
    );
    assert_eq!(one("SELECT user_id FROM audit"), 2);
    assert_eq!(
        one("SELECT bytes FROM audit_size WHERE id = 1"),
        size_before,
        "the audit size stays"
    );
    // The deleted rule's id is not reused, and the rebuilt audit table still counts its bytes.
    conn.execute(
        "INSERT INTO rules (scope, pattern_kind, pattern, effect, created_at, created_by, user_id)
         VALUES ('global', 'exact', 'b.example', 'allow', 1, 'ui', 1)",
        [],
    )
    .unwrap();
    assert_eq!(conn.last_insert_rowid(), 4);
    conn.execute(
        "INSERT INTO audit (ts, type, line, user_id) VALUES (2, 'x', 'abcd', 1)",
        [],
    )
    .unwrap();
    assert_eq!(
        one("SELECT bytes FROM audit_size WHERE id = 1"),
        size_before + 4
    );
    assert_eq!(
        one("SELECT count(*) FROM workspace_identities WHERE identity_id = 1"),
        1
    );
    // A write that names no user fails, in every one of the seven tables.
    for sql in [
        "INSERT INTO rules (scope, pattern_kind, pattern, effect, created_at, created_by)
         VALUES ('global', 'exact', 'c.example', 'allow', 1, 'ui')",
        "INSERT INTO request_rules (scope, host, methods, path, created_at, created_by)
         VALUES ('global', 'h.example', '*', '/*', 1, 'api')",
        "INSERT INTO audit (ts, type, line) VALUES (1, 'x', 'y')",
        "INSERT INTO rule_sets (name, created_at, created_by) VALUES ('Other', 1, 'ui')",
        "INSERT INTO rule_set_switches (rule_set, enabled, changed_at) VALUES ('1', 1, 1)",
        "INSERT INTO env_vars (name, kind, value, changed_at) VALUES ('B', 'plain', 'v', 1)",
        "INSERT INTO identities (label, author_name, author_email, credentials, position, created_at, changed_at)
         VALUES ('home', 'n', 'e@x', '[]', 1, 1, 1)",
    ] {
        assert!(conn.execute(sql, []).is_err(), "{sql}");
    }
    // The rule-to-set reference and the per-user names still hold.
    assert!(
        conn.execute(
            "INSERT INTO rules (scope, set_id, pattern_kind, pattern, effect, created_at, created_by, user_id)
             VALUES ('set', 99, 'exact', 'x.example', 'allow', 1, 'ui', 2)",
            [],
        )
        .is_err()
    );
    assert!(
        conn.execute(
            "INSERT INTO rule_sets (name, created_at, created_by, user_id) VALUES ('MINE', 1, 'ui', 2)",
            [],
        )
        .is_err()
    );
    assert_eq!(violations(&conn).unwrap(), []);
}

const PARENT_AND_CHILD: &str = "
CREATE TABLE parent (id INTEGER PRIMARY KEY, name TEXT NOT NULL);
CREATE TABLE child (id INTEGER PRIMARY KEY, parent_id INTEGER NOT NULL REFERENCES parent (id) ON DELETE CASCADE);
INSERT INTO parent VALUES (1, 'a'), (2, 'b');
INSERT INTO child VALUES (10, 1), (11, 2);
";

const REBUILD_PARENT: &str = "
CREATE TABLE parent_new (id INTEGER PRIMARY KEY, name TEXT NOT NULL, extra INTEGER NOT NULL DEFAULT 0);
INSERT INTO parent_new (id, name) SELECT id, name FROM parent;
DROP TABLE parent;
ALTER TABLE parent_new RENAME TO parent;
";

fn count(conn: &Connection, table: &str) -> i64 {
    conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
        row.get(0)
    })
    .unwrap()
}

fn foreign_keys_on(conn: &Connection) -> bool {
    conn.pragma_query_value(None, "foreign_keys", |row| row.get(0))
        .unwrap()
}

#[test]
fn a_table_rebuild_keeps_the_child_rows_and_foreign_keys_come_back_on() {
    let mut conn = Connection::open_in_memory().unwrap();
    conn.pragma_update(None, "foreign_keys", true).unwrap();
    migrate_with(&mut conn, &[PARENT_AND_CHILD]).unwrap();
    migrate_with(&mut conn, &[PARENT_AND_CHILD, REBUILD_PARENT]).unwrap();
    assert_eq!(count(&conn, "child"), 2);
    assert_eq!(count(&conn, "parent"), 2);
    assert!(foreign_keys_on(&conn));
    // Enforcement is live again after the migration.
    assert!(
        conn.execute("INSERT INTO child VALUES (12, 99)", [])
            .is_err()
    );
}

#[test]
fn a_step_that_orphans_rows_is_rolled_back_and_named() {
    let mut conn = Connection::open_in_memory().unwrap();
    conn.pragma_update(None, "foreign_keys", true).unwrap();
    let orphaning = "DELETE FROM parent WHERE id = 1;";
    let steps = [PARENT_AND_CHILD, orphaning];
    migrate_with(&mut conn, &steps[..1]).unwrap();
    assert!(matches!(
        migrate_with(&mut conn, &steps),
        Err(StoreError::MigrationBrokeForeignKeys { version: 2, ref table, ref parent })
            if table == "child" && parent == "parent"
    ));
    assert_eq!(version(&conn), 1);
    assert_eq!(count(&conn, "parent"), 2);
    assert!(foreign_keys_on(&conn));
}

#[test]
fn foreign_keys_stay_off_when_the_caller_had_them_off() {
    let mut conn = Connection::open_in_memory().unwrap();
    conn.pragma_update(None, "foreign_keys", false).unwrap();
    migrate_with(&mut conn, &[PARENT_AND_CHILD]).unwrap();
    assert!(!foreign_keys_on(&conn));
}

#[test]
fn an_older_orphan_does_not_block_an_unrelated_step() {
    let mut conn = Connection::open_in_memory().unwrap();
    migrate_with(&mut conn, &[PARENT_AND_CHILD]).unwrap();
    conn.pragma_update(None, "foreign_keys", false).unwrap();
    conn.execute("INSERT INTO child VALUES (13, 99)", [])
        .unwrap();
    migrate_with(
        &mut conn,
        &[PARENT_AND_CHILD, "CREATE TABLE other (x INTEGER);"],
    )
    .unwrap();
    assert_eq!(version(&conn), 2);
}

fn backups(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(".pre-v"))
        .collect();
    names.sort();
    names
}

fn steps_to(n: usize) -> Vec<String> {
    let mut steps = vec![PARENT_AND_CHILD.to_owned()];
    steps.extend((1..n).map(|i| format!("CREATE TABLE t{i} (x INTEGER);")));
    steps
}

fn migrate_to(conn: &mut Connection, n: usize) -> Result<u32, StoreError> {
    let owned = steps_to(n);
    let steps: Vec<&str> = owned.iter().map(String::as_str).collect();
    migrate_with(conn, &steps)
}

#[test]
fn the_database_is_copied_before_the_first_step_and_only_two_copies_stay() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("puddle.db");
    let mut conn = Connection::open(&path).unwrap();
    migrate_to(&mut conn, 1).unwrap();
    assert!(
        backups(dir.path()).is_empty(),
        "a new database has nothing to copy"
    );

    migrate_to(&mut conn, 2).unwrap();
    assert_eq!(backups(dir.path()), ["puddle.db.pre-v2"]);
    let copy = Connection::open(dir.path().join("puddle.db.pre-v2")).unwrap();
    assert_eq!(
        version(&copy),
        1,
        "the copy is the database as it was before"
    );
    assert_eq!(count(&copy, "child"), 2);
    // Windows refuses to delete a file that is still open, and step 4 prunes this copy.
    drop(copy);

    migrate_to(&mut conn, 3).unwrap();
    migrate_to(&mut conn, 4).unwrap();
    assert_eq!(
        backups(dir.path()),
        ["puddle.db.pre-v3", "puddle.db.pre-v4"]
    );
    migrate_to(&mut conn, 4).unwrap();
    assert_eq!(
        backups(dir.path()),
        ["puddle.db.pre-v3", "puddle.db.pre-v4"],
        "a database already current is not copied again"
    );
}

#[test]
fn a_failed_step_leaves_the_copy_and_a_rerun_replaces_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("puddle.db");
    let mut conn = Connection::open(&path).unwrap();
    migrate_to(&mut conn, 1).unwrap();
    let mut broken = steps_to(2);
    broken[1] = "NOT SQL;".to_owned();
    let refs: Vec<&str> = broken.iter().map(String::as_str).collect();
    assert!(migrate_with(&mut conn, &refs).is_err());
    assert_eq!(backups(dir.path()), ["puddle.db.pre-v2"]);
    migrate_to(&mut conn, 2).unwrap();
    assert_eq!(version(&conn), 2);
}

#[test]
fn opening_an_old_store_file_leaves_a_pre_version_copy() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("puddle.db");
    {
        let mut conn = Connection::open(&path).unwrap();
        migrate_up_to(&mut conn, 19).unwrap();
    }
    crate::Store::open(
        &path,
        std::sync::Arc::new(crate::clock::ManualClock::new(1)),
        crate::Limits::default(),
    )
    .unwrap();
    assert_eq!(
        backups(dir.path()),
        [format!("puddle.db.pre-v{SCHEMA_VERSION}")]
    );
}

#[test]
fn an_in_memory_database_is_never_copied_to_disk() {
    for name in ["", ":memory:"] {
        let mut conn = Connection::open(name).unwrap();
        migrate_to(&mut conn, 1).unwrap();
        migrate_to(&mut conn, 2).unwrap();
        assert_eq!(version(&conn), 2);
    }
    assert!(
        !Path::new(".pre-v2").exists() && !Path::new(":memory:.pre-v2").exists(),
        "a copy of a memory database landed in the working directory"
    );
}

#[test]
fn a_copy_that_cannot_be_pruned_is_named_in_the_error() {
    let dir = tempfile::tempdir().unwrap();
    let mut conn = Connection::open(dir.path().join("puddle.db")).unwrap();
    migrate_to(&mut conn, 1).unwrap();
    std::fs::create_dir(dir.path().join("puddle.db.pre-v1")).unwrap();
    migrate_to(&mut conn, 2).unwrap();
    let err = migrate_to(&mut conn, 3).unwrap_err().to_string();
    assert!(
        err.contains("pre-v1"),
        "the error blames the wrong file: {err}"
    );
}

#[test]
fn a_copy_that_cannot_be_replaced_stops_the_migration_before_any_step() {
    let dir = tempfile::tempdir().unwrap();
    let mut conn = Connection::open(dir.path().join("puddle.db")).unwrap();
    migrate_to(&mut conn, 1).unwrap();
    std::fs::create_dir(dir.path().join("puddle.db.pre-v2")).unwrap();
    assert!(matches!(
        migrate_to(&mut conn, 2),
        Err(StoreError::Backup(_))
    ));
    assert_eq!(version(&conn), 1);
}
