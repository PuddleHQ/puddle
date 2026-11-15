// SPDX-License-Identifier: AGPL-3.0-or-later
//! The audit is kept for a number of days: only records older than that go, nothing goes for size
//! or because of one workspace, and the file gives the space back.
#![expect(
    clippy::unwrap_used,
    reason = "test helpers outside #[test] functions fail the test by panicking"
)]

use std::sync::Arc;

use puddle_store::testing::own;
use puddle_store::{
    DEFAULT_AUDIT_RETENTION_DAYS, Limits, MAX_AUDIT_RETENTION_DAYS, ManualClock, Store, StoreError,
    SystemClock,
};
use puddle_types::{EgressRequest, Host, SuffixAllows, WorkspaceId};
use serde_json::Value;

const T0: u64 = 1_800_000_000_000;
const DAY: u64 = 24 * 60 * 60 * 1000;

fn ws(id: &str) -> WorkspaceId {
    WorkspaceId::new(id).unwrap()
}

fn fixture_with(limits: Limits) -> (Arc<ManualClock>, Store) {
    let clock = Arc::new(ManualClock::new(T0));
    let store = Store::open_in_memory(clock.clone(), limits).unwrap();
    for name in ["a", "b"] {
        own(&store, &ws(name));
    }
    (clock, store)
}

fn fixture() -> (Arc<ManualClock>, Store) {
    fixture_with(Limits::default())
}

/// Limits that let one workspace have thousands of open requests, so a test can write many records.
fn roomy() -> Limits {
    Limits {
        max_open_rows: 100_000,
        ..Limits::default()
    }
}

/// A request nobody has answered, so the store writes a record at the clock's time.
fn ask(store: &Store, workspace: &str, host: &str) {
    let request = EgressRequest::new(ws(workspace), Host::parse_normalised(host).unwrap(), 443);
    store.decide(&request, SuffixAllows::Count).unwrap();
}

fn audit(store: &Store) -> Vec<Value> {
    store
        .audit_lines(0, 1_000_000)
        .unwrap()
        .into_iter()
        .map(|(_, line)| serde_json::from_str(&line).unwrap())
        .collect()
}

fn mentions(store: &Store, host: &str) -> usize {
    audit(store)
        .iter()
        .filter(|v| v.to_string().contains(host))
        .count()
}

#[test]
fn the_retention_is_ninety_days_until_set_and_only_one_to_3650_days_are_taken() {
    let (_, store) = fixture();
    assert_eq!(DEFAULT_AUDIT_RETENTION_DAYS, 90);
    assert_eq!(store.audit_retention_days().unwrap(), 90);
    store.set_audit_retention_days(14).unwrap();
    assert_eq!(store.audit_retention_days().unwrap(), 14);
    store
        .set_audit_retention_days(MAX_AUDIT_RETENTION_DAYS)
        .unwrap();
    for bad in [0, MAX_AUDIT_RETENTION_DAYS + 1] {
        assert!(matches!(
            store.set_audit_retention_days(bad),
            Err(StoreError::AuditRetention(days)) if days == bad
        ));
    }
    assert_eq!(
        store.audit_retention_days().unwrap(),
        MAX_AUDIT_RETENTION_DAYS,
        "a refused value changes nothing"
    );
}

#[test]
fn the_retention_survives_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("puddle.db");
    let store = Store::open(&path, Arc::new(SystemClock), Limits::default()).unwrap();
    store.set_audit_retention_days(45).unwrap();
    drop(store);
    let store = Store::open(&path, Arc::new(SystemClock), Limits::default()).unwrap();
    assert_eq!(store.audit_retention_days().unwrap(), 45);
}

#[test]
fn a_prune_deletes_exactly_the_records_older_than_the_days() {
    let (clock, store) = fixture();
    ask(&store, "a", "at-limit.example");
    let at_limit = mentions(&store, "at-limit.example");
    assert!(at_limit > 0);
    // A record exactly 30 days old is kept; one millisecond older goes.
    clock.set(T0 + 30 * DAY);
    ask(&store, "a", "fresh.example");
    assert_eq!(store.prune_audit(30).unwrap().deleted, 0);
    assert_eq!(mentions(&store, "at-limit.example"), at_limit);
    clock.advance(1);
    let before = audit(&store).len();
    let pruned = store.prune_audit(30).unwrap();
    assert!(pruned.deleted > 0);
    assert_eq!(mentions(&store, "at-limit.example"), 0);
    assert!(mentions(&store, "fresh.example") > 0, "newer records stay");
    // What is left: the fresh records and the one that says what was deleted.
    let lines = audit(&store);
    assert_eq!(lines.len() as u64, before as u64 - pruned.deleted + 1);
    let trimmed = lines.last().unwrap();
    assert_eq!(trimmed["type"], "audit_trimmed");
    assert_eq!(trimmed["deleted_records"], pruned.deleted);
    assert_eq!(trimmed["oldest_ts_kept"], T0 + 30 * DAY);
}

#[test]
fn a_prune_with_nothing_old_deletes_nothing_and_writes_no_record() {
    let (_, store) = fixture();
    ask(&store, "a", "x.example");
    let before = audit(&store);
    assert_eq!(store.prune_audit(1).unwrap().deleted, 0);
    assert_eq!(audit(&store), before);
}

#[test]
fn a_prune_deletes_in_chunks_whatever_the_amount() {
    let (clock, store) = fixture_with(roomy());
    for n in 0..5200 {
        ask(&store, "a", &format!("h{n}.example"));
    }
    let old = audit(&store).len() as u64;
    assert!(old > 5000, "more than one chunk: {old}");
    clock.advance(10 * DAY);
    ask(&store, "a", "new.example");
    assert_eq!(store.prune_audit(5).unwrap().deleted, old);
    assert!(mentions(&store, "new.example") > 0);
}

#[test]
fn one_workspace_flooding_its_log_cannot_push_out_another_workspaces_records() {
    // The size only warns: even far over it, nothing is deleted while it is within the days.
    let (clock, store) = fixture_with(Limits {
        audit_warn_bytes: 1000,
        ..Limits::default()
    });
    ask(&store, "b", "quiet.example");
    for n in 0..400 {
        clock.advance(1);
        ask(&store, "a", &format!("flood{n}.example"));
    }
    assert!(store.audit_bytes().unwrap() > 1000);
    let before = audit(&store).len();
    let report = store.sweep().unwrap();
    assert_eq!(report.audit_records_pruned, 0);
    assert!(report.audit_over_warn_size);
    assert_eq!(audit(&store).len(), before);
    assert!(mentions(&store, "quiet.example") > 0);
}

#[test]
fn under_the_warning_size_the_sweep_does_not_warn() {
    let (_, store) = fixture();
    ask(&store, "a", "x.example");
    assert!(!store.sweep().unwrap().audit_over_warn_size);
}

#[test]
fn the_sweeper_prunes_by_the_saved_days_at_most_once_an_hour() {
    let (clock, store) = fixture();
    store.set_audit_retention_days(1).unwrap();
    ask(&store, "a", "old.example");
    clock.set(T0 + DAY - 30 * 60 * 1000);
    assert_eq!(store.sweep().unwrap().audit_records_pruned, 0);
    // The record is now over a day old, but the last look was 31 minutes ago.
    clock.set(T0 + DAY + 1000);
    assert_eq!(store.sweep().unwrap().audit_records_pruned, 0);
    assert!(mentions(&store, "old.example") > 0);
    clock.set(T0 + DAY + 31 * 60 * 1000);
    assert!(store.sweep().unwrap().audit_records_pruned > 0);
    assert_eq!(mentions(&store, "old.example"), 0);
}

#[test]
fn a_changed_retention_is_applied_at_the_next_sweep_not_after_the_hour() {
    let (clock, store) = fixture();
    ask(&store, "a", "old.example");
    clock.set(T0 + 10 * DAY);
    assert_eq!(store.sweep().unwrap().audit_records_pruned, 0);
    store.set_audit_retention_days(5).unwrap();
    assert!(store.sweep().unwrap().audit_records_pruned > 0);
    assert_eq!(mentions(&store, "old.example"), 0);
}

#[test]
fn the_stats_say_what_the_audit_holds_and_how_fast_it_grows() {
    let (clock, store) = fixture();
    let empty = store.audit_stats().unwrap();
    assert_eq!((empty.records, empty.bytes, empty.oldest_ts), (0, 0, None));
    assert_eq!(empty.retention_days, 90);
    assert_eq!(empty.warn_bytes, Limits::default().audit_warn_bytes);
    ask(&store, "a", "week-old.example");
    let first = store.audit_stats().unwrap();
    clock.set(T0 + 3 * DAY);
    ask(&store, "a", "three-days.example");
    clock.set(T0 + 7 * DAY + 1000);
    ask(&store, "a", "today.example");
    let stats = store.audit_stats().unwrap();
    assert_eq!(stats.records, audit(&store).len() as u64);
    assert_eq!(stats.bytes, store.audit_bytes().unwrap());
    assert_eq!(stats.oldest_ts, Some(T0));
    // The records of T0 are in neither window, those of day 3 only in the week's, today's in both.
    let lines = audit(&store);
    let now = T0 + 7 * DAY + 1000;
    let bytes_since = |ms: u64| -> (u64, u64) {
        let rows = store.audit_lines(0, 1_000_000).unwrap();
        rows.iter()
            .filter(|(_, line)| {
                serde_json::from_str::<Value>(line).unwrap()["ts"]
                    .as_u64()
                    .unwrap()
                    >= now - ms
            })
            .fold((0, 0), |(n, b), (_, line)| (n + 1, b + line.len() as u64))
    };
    assert_eq!(
        (stats.records_last_day, stats.bytes_last_day),
        bytes_since(DAY)
    );
    assert_eq!(
        (stats.records_last_week, stats.bytes_last_week),
        bytes_since(7 * DAY)
    );
    assert!(stats.records_last_day > 0 && stats.records_last_week > stats.records_last_day);
    assert_eq!(
        stats.records,
        first.records + stats.records_last_week,
        "{} lines",
        lines.len()
    );
    assert!(stats.file_bytes > 0);
}

fn auto_vacuum(path: &std::path::Path) -> i64 {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.pragma_query_value(None, "auto_vacuum", |row| row.get(0))
        .unwrap()
}

fn fill_and_prune(store: &Store, clock: &ManualClock, tag: &str) {
    for n in 0..3000 {
        ask(store, "a", &format!("{tag}{n}.example"));
    }
    clock.advance(10 * DAY);
    ask(store, "a", &format!("{tag}-kept.example"));
    assert!(store.prune_audit(5).unwrap().deleted >= 3000);
}

#[test]
fn the_file_gives_the_space_of_pruned_records_back_in_steps() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("puddle.db");
    let clock = Arc::new(ManualClock::new(T0));
    let store = Store::open(&path, clock.clone(), roomy()).unwrap();
    own(&store, &ws("a"));
    assert_eq!(auto_vacuum(&path), 2, "a new file is made incremental");
    fill_and_prune(&store, &clock, "first");
    let full = store.audit_stats().unwrap();
    assert!(full.free_bytes * 4 > full.file_bytes, "{full:?}");
    let shrunk = store.shrink_audit_file().unwrap();
    let after = store.audit_stats().unwrap();
    assert_eq!(after.free_bytes, 0);
    assert_eq!(full.file_bytes - after.file_bytes, shrunk);
    assert!(shrunk > 0);
    // Nothing is free now, so the next call does nothing.
    assert_eq!(store.shrink_audit_file().unwrap(), 0);
    // The records that stayed are all still there.
    assert!(mentions(&store, "first-kept.example") > 0);
    // A second round works the same way, with no rebuild this time.
    fill_and_prune(&store, &clock, "second");
    assert!(store.shrink_audit_file().unwrap() > 0);
    assert_eq!(store.audit_stats().unwrap().free_bytes, 0);
    assert!(mentions(&store, "second-kept.example") > 0);
}

#[test]
fn an_older_file_that_was_not_incremental_is_rebuilt_once_when_it_has_space_to_give() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("puddle.db");
    let clock = Arc::new(ManualClock::new(T0));
    drop(Store::open(&path, clock.clone(), Limits::default()).unwrap());
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.pragma_update(None, "auto_vacuum", 0).unwrap();
        conn.execute("VACUUM", []).unwrap();
    }
    assert_eq!(auto_vacuum(&path), 0);
    let store = Store::open(&path, clock.clone(), roomy()).unwrap();
    own(&store, &ws("a"));
    assert_eq!(store.shrink_audit_file().unwrap(), 0, "nothing free yet");
    assert_eq!(auto_vacuum(&path), 0, "no space to give, so no rebuild");
    fill_and_prune(&store, &clock, "old");
    assert!(store.shrink_audit_file().unwrap() > 0);
    assert_eq!(auto_vacuum(&path), 2);
    assert!(mentions(&store, "old-kept.example") > 0);
}
