// SPDX-License-Identifier: AGPL-3.0-or-later
//! An install that upgrades: a database an older Puddle wrote (schema version 23, checked in as
//! `tests/data/golden-v23.db`, every table filled) is opened by this build. Every row has to read
//! back with the values it had, in the raw tables and through the store's own reads.
//!
//! The file is made by `schema::tests::write_golden_database` from `golden-v23-rows.sql`. Do not
//! rebuild it with a newer schema: the point is a database that is old.
#![expect(
    clippy::unwrap_used,
    reason = "test helpers outside #[test] functions fail the test by panicking"
)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use puddle_store::{
    Actor, Effect, EnvScope, EnvValue, Limits, ManualClock, SCHEMA_VERSION, Store, UserId,
};
use puddle_types::{PendingId, WorkspaceId};
use rusqlite::{Connection, types::Value};

type Row = Vec<(String, Value)>;
type Tables = BTreeMap<String, Vec<Row>>;

const GOLDEN_VERSION: i64 = 23;

/// Opening the store records each built-in set it has not seen yet, so this table gains rows (the
/// golden ones are for sets no longer shipped, which opening leaves alone) and its old rows must
/// all still be there.
const GROWS_ON_OPEN: &str = "builtin_sets_seen";

fn golden_copy() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("puddle.db");
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/golden-v23.db");
    std::fs::copy(source, &path).unwrap();
    (dir, path)
}

fn open_store(path: &Path) -> Store {
    let clock = Arc::new(ManualClock::new(1_800_000_000_000));
    Store::open(path, clock, Limits::default()).unwrap()
}

/// Every table of the database, every row in rowid order, every column by name.
fn snapshot(path: &Path) -> Tables {
    let conn = Connection::open(path).unwrap();
    let names: Vec<String> = conn
        .prepare(
            "SELECT name FROM sqlite_master
             WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
        )
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    names
        .into_iter()
        .map(|table| {
            let mut stmt = conn
                .prepare(&format!("SELECT * FROM \"{table}\" ORDER BY rowid"))
                .unwrap();
            let columns: Vec<String> = stmt
                .column_names()
                .iter()
                .map(|c| (*c).to_owned())
                .collect();
            let rows = stmt
                .query_map([], |r| {
                    columns
                        .iter()
                        .enumerate()
                        .map(|(i, c)| Ok((c.clone(), r.get::<_, Value>(i)?)))
                        .collect::<Result<Row, _>>()
                })
                .unwrap()
                .map(Result::unwrap)
                .collect();
            (table, rows)
        })
        .collect()
}

fn sequences(path: &Path) -> BTreeMap<String, i64> {
    let conn = Connection::open(path).unwrap();
    conn.prepare("SELECT name, seq FROM sqlite_sequence")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn user_version(path: &Path) -> i64 {
    Connection::open(path)
        .unwrap()
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap()
}

fn ws(name: &str) -> WorkspaceId {
    WorkspaceId::new(name).unwrap()
}

#[test]
fn the_golden_database_is_old_and_every_table_has_rows() {
    let (_dir, path) = golden_copy();
    assert_eq!(user_version(&path), GOLDEN_VERSION);
    let empty: Vec<String> = snapshot(&path)
        .into_iter()
        .filter(|(_, rows)| rows.is_empty())
        .map(|(table, _)| table)
        .collect();
    assert_eq!(empty, Vec::<String>::new(), "tables with no golden row");
    assert!(
        GOLDEN_VERSION < i64::from(SCHEMA_VERSION),
        "nothing left to migrate"
    );
}

/// Table by table, row by row: nothing is lost, and no column that was filled reads differently.
/// A column the migrations dropped or a table they removed fails too, so changing the stored
/// shape on purpose means changing the golden test with it.
#[test]
fn every_row_of_the_old_database_reads_back_equal_after_the_migration() {
    let (_dir, path) = golden_copy();
    let before = snapshot(&path);
    let sequences_before = sequences(&path);
    drop(open_store(&path));
    assert_eq!(user_version(&path), i64::from(SCHEMA_VERSION));
    let after = snapshot(&path);

    let mut differences = Vec::new();
    for (table, rows_before) in &before {
        let Some(rows_after) = after.get(table) else {
            differences.push(format!("{table}: table gone"));
            continue;
        };
        if table == GROWS_ON_OPEN {
            for old in rows_before.iter().filter(|old| !rows_after.contains(old)) {
                differences.push(format!("{table}: row lost or changed: {old:?}"));
            }
            continue;
        }
        if rows_before.len() != rows_after.len() {
            differences.push(format!(
                "{table}: {} rows became {}",
                rows_before.len(),
                rows_after.len()
            ));
            continue;
        }
        // flake-ok: both snapshots read in rowid order, and a migration copies rows in that order
        for (n, (old, new)) in rows_before.iter().zip(rows_after).enumerate() {
            for (column, value) in old {
                match new.iter().find(|(c, _)| c == column) {
                    None => differences.push(format!("{table} row {n}: column {column} gone")),
                    Some((_, now)) if now != value => differences.push(format!(
                        "{table} row {n}: {column} was {value:?}, is {now:?}"
                    )),
                    Some(_) => {}
                }
            }
        }
    }
    assert_eq!(differences, Vec::<String>::new());

    // A deleted row's id stays spent: the counters never move back.
    let sequences_after = sequences(&path);
    for (table, seq) in sequences_before {
        let now = sequences_after.get(&table).copied().unwrap_or(0);
        assert!(now >= seq, "{table}: counter went from {seq} to {now}");
    }
}

/// The columns the migrations added hold what an old row is documented to read as.
#[test]
fn the_columns_added_since_read_as_their_documented_defaults() {
    let (_dir, path) = golden_copy();
    drop(open_store(&path));
    let conn = Connection::open(&path).unwrap();
    let count = |sql: &str| -> i64 { conn.query_row(sql, [], |r| r.get(0)).unwrap() };
    assert_eq!(count("SELECT count(*) FROM rules WHERE precedence = 5"), 6);
    assert_eq!(
        count("SELECT count(*) FROM request_rules WHERE precedence = 5"),
        2
    );
    assert_eq!(
        count("SELECT count(*) FROM rule_sets WHERE default_precedence IS NULL"),
        2
    );
    assert_eq!(
        count("SELECT count(*) FROM ssh_agent_hosts WHERE host_key IS NULL"),
        2
    );
    assert_eq!(
        count("SELECT count(*) FROM pending WHERE exact_only = 0"),
        5
    );
}

/// The same data through the reads the host and the API use, with literal expected values.
#[test]
fn the_migrated_database_serves_every_old_row_through_the_store() {
    let (_dir, path) = golden_copy();
    let store = open_store(&path);
    let owner = UserId::OWNER;
    let ann = UserId::from_stored(2);

    let users: Vec<(i64, String)> = store
        .users()
        .unwrap()
        .iter()
        .map(|u| (u.id.get(), u.name.as_str().to_owned()))
        .collect();
    assert_eq!(
        users,
        [
            (1, "tijs".to_owned()),
            (2, "Ann".to_owned()),
            (3, "bob".to_owned())
        ]
    );

    // Rules: ids, scope, effect, expiry, author and origin all as stored.
    let rules = store.rules(owner);
    let ids: Vec<i64> = rules.iter().map(|r| r.id.0).collect();
    assert!(
        ids.contains(&1) && ids.contains(&2) && ids.contains(&3),
        "{ids:?}"
    );
    let rule = |id: i64| rules.iter().find(|r| r.id.0 == id).unwrap();
    assert_eq!(rule(1).pattern.to_string(), "api.example.com");
    assert_eq!(rule(1).effect, Effect::Allow);
    assert_eq!(rule(1).created_at, 1_700_002_000);
    assert_eq!(rule(1).created_by, Actor::Ui);
    assert_eq!(rule(2).effect, Effect::Deny);
    assert_eq!(rule(2).created_by, Actor::Cli);
    assert_eq!(rule(3).expires_at, Some(1_900_000_000));
    assert_eq!(rule(3).created_by, Actor::Api);
    assert_eq!(rule(3).source_pending_id, Some(PendingId(2)));
    assert_eq!(rule(3).precedence.get(), 5);
    assert!(rules.iter().all(|r| r.enabled));
    let anns = store.rules(ann);
    assert!(
        anns.iter()
            .any(|r| r.id.0 == 5 && r.created_by == Actor::WorkspaceCreate)
    );

    // Pending requests, one of each state, with who decided them.
    let waiting = store.pending(PendingId(5)).unwrap();
    assert_eq!(waiting.workspace, ws("ws-alpha"));
    assert_eq!(waiting.port, 443);
    assert_eq!(
        (waiting.first_seen, waiting.last_seen, waiting.attempts),
        (1_700_006_040, 1_700_006_140, 4)
    );
    assert_eq!(waiting.decided_at, None);
    let allowed = store.pending(PendingId(2)).unwrap();
    assert_eq!(allowed.decided_at, Some(1_700_006_210));
    assert_eq!(allowed.decided_by, Some(Actor::Cli));
    assert_eq!(allowed.rule_id.map(|r| r.0), Some(3));
    assert_eq!(allowed.attempts, 2);
    let denied = store.pending(PendingId(3)).unwrap();
    assert_eq!(denied.attempts, 7);
    assert_eq!(denied.decided_by, Some(Actor::Api));

    // Settings.
    let settings = store.interception_settings().unwrap();
    assert!(settings.mode.is_some() && settings.learn_from_rejections && settings.notice_seen);

    // Identities, with their credentials, and the workspaces they are attached to.
    let identities = store.identities(owner).unwrap();
    let labels: Vec<&str> = identities.iter().map(|i| i.label.as_str()).collect();
    assert_eq!(labels, ["Work", "home"]);
    assert!(identities[0].is_default && !identities[1].is_default);
    assert_eq!(identities[0].credentials.len(), 1);
    assert_eq!(identities[0].credentials[0].host.as_str(), "github.com");
    assert_eq!(identities[0].created_at, 1_700_009_000);
    assert_eq!(identities[0].changed_at, 1_700_009_100);
    assert_eq!(identities[1].credentials.len(), 0);
    assert_eq!(
        store.identity_workspaces(identities[0].id).unwrap(),
        [ws("ws-alpha")]
    );
    assert_eq!(store.identities(ann).unwrap().len(), 1);

    // Environment variables: a plain one, and a secret that keeps its reference and hosts.
    let global = store.env_entries(owner, &EnvScope::Global).unwrap();
    assert_eq!(global.len(), 1);
    assert_eq!(global[0].name.as_str(), "EDITOR");
    assert_eq!(global[0].value, EnvValue::Plain("vim".to_owned()));
    let alpha = store
        .env_entries(owner, &EnvScope::Workspace(ws("ws-alpha")))
        .unwrap();
    assert_eq!(alpha.len(), 1);
    let secret = alpha[0].value.as_secret().unwrap();
    assert_eq!(secret.id.as_str(), "secret-gh");
    assert_eq!(secret.hosts.len(), 1);
    assert_eq!(alpha[0].changed_at, 1_700_010_010);
}
