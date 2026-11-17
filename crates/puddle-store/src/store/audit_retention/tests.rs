// SPDX-License-Identifier: AGPL-3.0-or-later
//! The edges of the file shrink's rule, which the public call fixes at 64 MiB.
use std::sync::Arc;

use puddle_types::{EgressRequest, Host, SuffixAllows, WorkspaceId};

use super::*;
use crate::clock::ManualClock;
use crate::store::Limits;

const DAY: u64 = 24 * 60 * 60 * 1000;

fn auto_vacuum(path: &std::path::Path) -> i64 {
    let conn = Connection::open(path).unwrap();
    conn.pragma_query_value(None, "auto_vacuum", |row| row.get(0))
        .unwrap()
}

/// A file store whose few oldest records were pruned: a little of the file is free, far less
/// than a quarter.
fn store_with_a_little_free(path: &std::path::Path, tag: &str) -> Store {
    let clock = Arc::new(ManualClock::new(1_800_000_000_000));
    let limits = Limits {
        max_open_rows: 100_000,
        ..Limits::default()
    };
    let store = Store::open(path, clock.clone(), limits).unwrap();
    let workspace = WorkspaceId::new("a").unwrap();
    crate::testing::own(&store, &workspace);
    let ask = |n: u32| {
        let host = Host::parse_normalised(&format!("{tag}{n}.example")).unwrap();
        let request = EgressRequest::new(workspace.clone(), host, 443);
        store.decide(&request, SuffixAllows::Count).unwrap();
    };
    for n in 0..40 {
        ask(n);
    }
    clock.advance(10 * DAY);
    for n in 40..3000 {
        ask(n);
    }
    assert!(store.prune_audit(5).unwrap().deleted >= 40);
    store
}

#[test]
fn a_file_with_a_little_free_space_shrinks_only_once_that_reaches_the_byte_threshold() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("puddle.db");
    let store = store_with_a_little_free(&path, "h");
    let stats = store.audit_stats().unwrap();
    assert!(stats.free_bytes > 0, "{stats:?}");
    assert!(stats.free_bytes * 100 < stats.file_bytes * 25, "{stats:?}");
    // One byte more than is free, and under a quarter: nothing happens.
    assert_eq!(store.shrink_file(stats.free_bytes + 1).unwrap(), 0);
    assert_eq!(store.audit_stats().unwrap().free_bytes, stats.free_bytes);
    // Exactly what is free is enough.
    assert!(store.shrink_file(stats.free_bytes).unwrap() > 0);
    assert_eq!(store.audit_stats().unwrap().free_bytes, 0);
    // Nothing free now: nothing to do, whatever the threshold.
    assert_eq!(store.shrink_file(0).unwrap(), 0);
}

#[test]
fn an_older_file_with_little_free_space_is_not_rebuilt_on_open() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("puddle.db");
    let store = store_with_a_little_free(&path, "first");
    {
        let conn = lock(&store.conn);
        conn.pragma_update(None, "auto_vacuum", 0).unwrap();
        conn.execute("VACUUM", []).unwrap();
    }
    drop(store);
    let store = store_with_a_little_free(&path, "second");
    let stats = store.audit_stats().unwrap();
    assert!(stats.free_bytes > 0, "{stats:?}");
    drop(store);
    let store = Store::open(&path, Arc::new(ManualClock::new(0)), Limits::default()).unwrap();
    assert_eq!(auto_vacuum(&path), 0, "under the threshold, so no rebuild");
    assert_eq!(store.audit_stats().unwrap(), stats);
}

/// A file store, incremental as every new file is, with most of its file free after a prune.
fn store_with_much_free(path: &std::path::Path, limits: Limits, tag: &str) -> Store {
    let clock = Arc::new(ManualClock::new(1_800_000_000_000));
    let store = Store::open(path, clock.clone(), limits).unwrap();
    let workspace = WorkspaceId::new("a").unwrap();
    crate::testing::own(&store, &workspace);
    for n in 0..3000 {
        let host = Host::parse_normalised(&format!("{tag}{n}.example")).unwrap();
        let request = EgressRequest::new(workspace.clone(), host, 443);
        store.decide(&request, SuffixAllows::Count).unwrap();
    }
    clock.advance(10 * DAY);
    assert!(store.prune_audit(5).unwrap().deleted >= 3000);
    let stats = store.audit_stats().unwrap();
    assert!(stats.free_bytes * 4 > stats.file_bytes, "{stats:?}");
    store
}

#[test]
fn a_shrink_that_fails_still_leaves_the_size_warning_in_the_sweep_report() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("puddle.db");
    let store = store_with_much_free(
        &path,
        Limits {
            max_open_rows: 100_000,
            audit_warn_bytes: 1,
            ..Limits::default()
        },
        "h",
    );
    store.refuse_pragma("incremental_vacuum");
    let report = store.sweep().unwrap();
    assert!(report.audit_over_warn_size, "{report:?}");
    assert_eq!(report.audit_file_shrunk_bytes, 0);
}

#[test]
fn the_sweeper_never_rebuilds_a_whole_file_and_the_next_open_does() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("puddle.db");
    let limits = Limits {
        max_open_rows: 100_000,
        ..Limits::default()
    };
    let store = store_with_much_free(&path, limits.clone(), "first");
    {
        let conn = lock(&store.conn);
        conn.pragma_update(None, "auto_vacuum", 0).unwrap();
        conn.execute("VACUUM", []).unwrap();
    }
    // Free space again, in a file that is not incremental.
    drop(store);
    let store = store_with_much_free(&path, limits.clone(), "second");
    assert_eq!(auto_vacuum(&path), 0);
    let stats = store.audit_stats().unwrap();
    assert_eq!(store.shrink_file(0).unwrap(), 0);
    assert_eq!(
        auto_vacuum(&path),
        0,
        "a rebuild holds the writer for the whole file"
    );
    assert_eq!(store.audit_stats().unwrap(), stats);
    drop(store);
    // Before anything is served, an open gives the space back once and makes the file
    // incremental.
    let store = Store::open(&path, Arc::new(ManualClock::new(0)), limits).unwrap();
    assert_eq!(auto_vacuum(&path), 2);
    let after = store.audit_stats().unwrap();
    assert_eq!(after.free_bytes, 0);
    assert!(after.file_bytes < stats.file_bytes, "{after:?} {stats:?}");
}

#[test]
fn an_open_whose_rebuild_fails_logs_it_and_goes_on() {
    use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
    let conn = Connection::open_in_memory().unwrap();
    conn.authorizer(Some(|context: AuthContext<'_>| match context.action {
        AuthAction::Pragma {
            pragma_name: "auto_vacuum",
            ..
        } => Authorization::Deny,
        _ => Authorization::Allow,
    }))
    .unwrap();
    let logs = crate::test_support::Logs::capture();
    rebuild_older_file_on_open(&conn);
    let lines = logs.lines();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(
        lines[0]
            .starts_with("WARN audit database file not rebuilt; the next open tries again error="),
        "{lines:?}"
    );
}
