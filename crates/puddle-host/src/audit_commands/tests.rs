// SPDX-License-Identifier: AGPL-3.0-or-later
use std::sync::Arc;

use puddle_store::{Limits, Store, SystemClock, UserId};

use super::*;

fn data_with_owner() -> (tempfile::TempDir, HostPaths) {
    let dir = tempfile::tempdir().unwrap();
    let paths = HostPaths::new(dir.path().to_path_buf());
    let store = Store::open(&paths.store(), Arc::new(SystemClock), Limits::default()).unwrap();
    drop(store);
    (dir, paths)
}

/// The activity log's records, newest first.
fn audit_records(paths: &HostPaths) -> Vec<serde_json::Value> {
    Store::open(&paths.store(), Arc::new(SystemClock), Limits::default())
        .unwrap()
        .audit_query(
            UserId::OWNER,
            &puddle_store::AuditFilter::default(),
            puddle_store::AuditCursor::Before(None),
            100,
        )
        .unwrap()
        .iter()
        .map(|(_, line)| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn a_prune_by_hand_is_recorded_with_the_days_and_the_count_and_a_refused_one_is_not() {
    let (_dir, paths) = data_with_owner();
    let done = prune_audit(&paths, Some(30)).unwrap();
    assert_eq!((done.days, done.deleted), (30, 0));
    let records = audit_records(&paths);
    let line = records.first().expect("a record of the prune");
    assert_eq!(line["type"], "workspace_op");
    assert_eq!(line["op"], "audit_pruned");
    assert_eq!(line["actor"], "cli");
    assert_eq!(
        line["detail"],
        "records older than 30 days deleted by hand: 0 records"
    );
    assert!(prune_audit(&paths, Some(0)).is_err());
    assert_eq!(audit_records(&paths).len(), records.len());
}
