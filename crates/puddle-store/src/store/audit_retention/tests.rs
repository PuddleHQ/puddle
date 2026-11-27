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
    store_with_records_pruned(path, limits, tag, 3000)
}

/// [`store_with_much_free`] with `records` decisions written, then all pruned.
fn store_with_records_pruned(
    path: &std::path::Path,
    limits: Limits,
    tag: &str,
    records: u32,
) -> Store {
    let clock = Arc::new(ManualClock::new(1_800_000_000_000));
    let store = Store::open(path, clock.clone(), limits).unwrap();
    let workspace = WorkspaceId::new("a").unwrap();
    crate::testing::own(&store, &workspace);
    for n in 0..records {
        let host = Host::parse_normalised(&format!("{tag}{n}.example")).unwrap();
        let request = EgressRequest::new(workspace.clone(), host, 443);
        store.decide(&request, SuffixAllows::Count).unwrap();
    }
    clock.advance(10 * DAY);
    assert!(store.prune_audit(5).unwrap().deleted >= u64::from(records));
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
fn a_log_truncation_that_fails_does_not_fail_the_sweep() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(
        &dir.path().join("puddle.db"),
        Arc::new(ManualClock::new(1_800_000_000_000)),
        Limits::default(),
    )
    .unwrap();
    store.refuse_pragma("wal_checkpoint");
    assert!(store.checkpoint_wal().is_err());
    store.sweep().unwrap();
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

fn wal_bytes(path: &std::path::Path) -> u64 {
    let mut wal = path.as_os_str().to_owned();
    wal.push("-wal");
    std::fs::metadata(wal).map_or(0, |meta| meta.len())
}

#[test]
fn a_sweep_empties_a_write_ahead_log_that_a_long_reader_let_grow() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("puddle.db");
    let clock = Arc::new(ManualClock::new(1_800_000_000_000));
    let limits = Limits {
        max_open_rows: 100_000,
        ..Limits::default()
    };
    let store = Store::open(&path, clock, limits).unwrap();
    let workspace = WorkspaceId::new("a").unwrap();
    crate::testing::own(&store, &workspace);
    // A reader that holds one snapshot open keeps every checkpoint from reaching the log's end,
    // so a burst of commits grows the log instead of reusing it.
    let reader = Connection::open(&path).unwrap();
    reader.execute_batch("BEGIN").unwrap();
    reader
        .query_row("SELECT count(*) FROM audit", [], |row| row.get::<_, i64>(0))
        .unwrap();
    for n in 0..3000 {
        let host = Host::parse_normalised(&format!("burst{n}.example")).unwrap();
        let request = EgressRequest::new(workspace.clone(), host, 443);
        store.decide(&request, SuffixAllows::Count).unwrap();
    }
    let peak = wal_bytes(&path);
    assert!(
        peak > 2 * WAL_LIMIT_BYTES as u64,
        "the burst must outgrow the cap: {peak}"
    );
    reader.execute_batch("COMMIT").unwrap();
    store.sweep().unwrap();
    let after = wal_bytes(&path);
    assert!(
        after < peak,
        "the sweep left the log at {after}, its peak was {peak}"
    );
    assert!(after < WAL_LIMIT_BYTES as u64, "{after}");
}

#[test]
fn a_shrink_frees_all_its_pages_and_reports_the_bytes_it_freed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("puddle.db");
    let limits = Limits {
        max_open_rows: 100_000,
        ..Limits::default()
    };
    let store = store_with_records_pruned(&path, limits, "h", 9000);
    let page_size = pragma_u64(&lock(&store.conn), "page_size").unwrap();
    let free_pages = pragma_u64(&lock(&store.conn), "freelist_count").unwrap();
    assert!(
        free_pages > u64::from(SHRINK_STEPS_PER_SWEEP),
        "{free_pages} free pages: the test needs more than one page per step could free"
    );
    assert!(
        free_pages < u64::from(SHRINK_STEP_PAGES),
        "{free_pages} free pages"
    );
    let pages_before = pragma_u64(&lock(&store.conn), "page_count").unwrap();
    let shrunk = store.shrink_file(0).unwrap();
    let pages_after = pragma_u64(&lock(&store.conn), "page_count").unwrap();
    assert_eq!(shrunk, (pages_before - pages_after) * page_size);
    // Every free page went, and the pointer-map pages that covered them with it.
    assert!(
        shrunk >= free_pages * page_size,
        "{shrunk} bytes, {free_pages} free pages"
    );
    assert_eq!(pragma_u64(&lock(&store.conn), "freelist_count").unwrap(), 0);
}

fn a_store_with_old_records_only() -> (Arc<ManualClock>, Store) {
    let clock = Arc::new(ManualClock::new(1_800_000_000_000));
    let store = Store::open_in_memory(clock.clone(), Limits::default()).unwrap();
    let workspace = WorkspaceId::new("a").unwrap();
    crate::testing::own(&store, &workspace);
    let host = Host::parse_normalised("x.example").unwrap();
    store
        .decide(
            &EgressRequest::new(workspace, host, 443),
            SuffixAllows::Count,
        )
        .unwrap();
    store.set_audit_retention_days(1).unwrap();
    (clock, store)
}

#[test]
fn a_sweep_that_would_delete_the_whole_audit_waits_for_a_second_pass_to_confirm() {
    let (clock, store) = a_store_with_old_records_only();
    let records = store.audit_stats().unwrap().records;
    assert!(records > 0);
    // The clock stepped forward past the retention: every record looks old.
    clock.advance(10 * DAY);

    assert_eq!(store.sweep().unwrap().audit_records_pruned, 0);
    assert_eq!(store.audit_stats().unwrap().records, records, "kept");
    // Within the hour nothing looks again.
    assert_eq!(store.sweep().unwrap().audit_records_pruned, 0);

    clock.advance(2 * 60 * 60 * 1000);
    assert_eq!(store.sweep().unwrap().audit_records_pruned, records);
}

#[test]
fn a_clock_that_steps_back_before_the_second_pass_cancels_the_deferred_delete() {
    let (clock, store) = a_store_with_old_records_only();
    let records = store.audit_stats().unwrap().records;
    clock.advance(10 * DAY);
    store.sweep().unwrap();
    // The step was wrong; a record now survives the cutoff, so a later pass starts over.
    clock.set(1_800_000_000_000 + 1000);
    clock.advance(2 * 60 * 60 * 1000);
    assert_eq!(store.sweep().unwrap().audit_records_pruned, 0);
    clock.advance(10 * DAY);
    clock.advance(2 * 60 * 60 * 1000);
    assert_eq!(
        store.sweep().unwrap().audit_records_pruned,
        0,
        "all old again, so it waits again"
    );
    assert_eq!(store.audit_stats().unwrap().records, records);
}

#[test]
fn a_manual_prune_deletes_at_once() {
    let (clock, store) = a_store_with_old_records_only();
    let records = store.audit_stats().unwrap().records;
    clock.advance(10 * DAY);
    assert_eq!(store.prune_audit(1).unwrap().deleted, records);
}

#[test]
fn a_prune_logs_what_it_deleted_and_stays_quiet_when_it_deleted_nothing() {
    let (clock, store) = a_store_with_old_records_only();
    let records = store.audit_stats().unwrap().records;
    let logs = crate::test_support::Logs::capture();

    // Nothing is past retention yet.
    assert_eq!(store.prune_audit(1).unwrap().deleted, 0);
    assert!(
        !logs
            .lines()
            .iter()
            .any(|line| line.contains("audit records past their retention deleted")),
        "a prune that deleted nothing says nothing: {:?}",
        logs.lines()
    );

    clock.advance(10 * DAY);
    assert_eq!(store.prune_audit(1).unwrap().deleted, records);
    let lines = logs.lines();
    let line = lines
        .iter()
        .find(|line| line.contains("audit records past their retention deleted"))
        .unwrap_or_else(|| panic!("a prune that deleted records says so: {lines:?}"));
    assert!(line.contains(&format!("deleted={records}")), "{line}");
}
