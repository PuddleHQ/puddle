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

/// A file store, made not incremental, whose few oldest records were pruned: a little of the
/// file is free, far less than a quarter.
fn store_with_a_little_free(path: &std::path::Path) -> Store {
    let clock = Arc::new(ManualClock::new(1_800_000_000_000));
    let limits = Limits {
        max_open_rows: 100_000,
        ..Limits::default()
    };
    drop(Store::open(path, clock.clone(), limits.clone()).unwrap());
    {
        let conn = Connection::open(path).unwrap();
        conn.pragma_update(None, "auto_vacuum", 0).unwrap();
        conn.execute("VACUUM", []).unwrap();
    }
    let store = Store::open(path, clock.clone(), limits).unwrap();
    let workspace = WorkspaceId::new("a").unwrap();
    crate::testing::own(&store, &workspace);
    let ask = |n: u32| {
        let host = Host::parse_normalised(&format!("h{n}.example")).unwrap();
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
    let store = store_with_a_little_free(&path);
    let stats = store.audit_stats().unwrap();
    assert!(stats.free_bytes > 0, "{stats:?}");
    assert!(stats.free_bytes * 100 < stats.file_bytes * 25, "{stats:?}");
    // One byte more than is free, and under a quarter: nothing happens, the file is not rebuilt.
    assert_eq!(store.shrink_file(stats.free_bytes + 1).unwrap(), 0);
    assert_eq!(auto_vacuum(&path), 0);
    assert_eq!(store.audit_stats().unwrap().free_bytes, stats.free_bytes);
    // Exactly what is free is enough.
    assert!(store.shrink_file(stats.free_bytes).unwrap() > 0);
    assert_eq!(auto_vacuum(&path), 2);
    assert_eq!(store.audit_stats().unwrap().free_bytes, 0);
}

#[test]
fn a_file_with_nothing_free_is_left_alone_whatever_the_threshold() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("puddle.db");
    let store = store_with_a_little_free(&path);
    store.shrink_file(0).unwrap();
    assert_eq!(store.audit_stats().unwrap().free_bytes, 0);
    {
        let conn = lock(&store.conn);
        conn.pragma_update(None, "auto_vacuum", 0).unwrap();
        conn.execute("VACUUM", []).unwrap();
    }
    assert_eq!(auto_vacuum(&path), 0);
    assert_eq!(store.shrink_file(0).unwrap(), 0);
    assert_eq!(auto_vacuum(&path), 0, "nothing to give back, so no rebuild");
}
