// SPDX-License-Identifier: AGPL-3.0-or-later
use std::sync::Arc;

use super::*;
use crate::clock::ManualClock;
use crate::store::{LearnedLimits, Limits};

fn ws(text: &str) -> WorkspaceId {
    WorkspaceId::new(text).unwrap()
}

fn store() -> (Arc<ManualClock>, Store) {
    let clock = Arc::new(ManualClock::new(1_000));
    let store = Store::open_in_memory(clock.clone(), Limits::default()).unwrap();
    (clock, store)
}

#[test]
fn a_refusal_makes_a_row_and_each_later_one_raises_the_count_and_the_last_time() {
    let (clock, store) = store();
    let first = store.record_rejection(&ws("a"), "pin.example").unwrap();
    assert_eq!(
        first,
        LearnedRow {
            host: "pin.example".into(),
            first_at: 1_000,
            last_at: 1_000,
            count: 1
        }
    );
    clock.advance(5_000);
    let second = store.record_rejection(&ws("a"), "pin.example").unwrap();
    assert_eq!(
        (second.first_at, second.last_at, second.count),
        (1_000, 6_000, 2)
    );
    assert_eq!(
        store.learned_history(&ws("a")).unwrap(),
        vec![second.clone()]
    );
    assert_eq!(
        store.learned_host(&ws("a"), "pin.example").unwrap(),
        Some(second)
    );
    assert_eq!(store.learned_host(&ws("b"), "pin.example").unwrap(), None);
    assert_eq!(store.learned_history(&ws("b")).unwrap(), []);
}

#[test]
fn the_history_lists_the_most_recent_refusal_first() {
    let (clock, store) = store();
    store.record_rejection(&ws("a"), "old.example").unwrap();
    clock.advance(10);
    store.record_rejection(&ws("a"), "new.example").unwrap();
    clock.advance(10);
    store.record_rejection(&ws("a"), "old.example").unwrap();
    let hosts: Vec<_> = store
        .learned_history(&ws("a"))
        .unwrap()
        .into_iter()
        .map(|r| (r.host, r.count))
        .collect();
    assert_eq!(
        hosts,
        [("old.example".to_owned(), 2), ("new.example".to_owned(), 1)]
    );
}

#[test]
fn past_the_cap_the_host_refused_longest_ago_makes_room() {
    let (clock, store) = store();
    store
        .set_learned_limits(LearnedLimits {
            ttl_secs: 3600,
            cap: 2,
        })
        .unwrap();
    for host in ["a.example", "b.example", "c.example"] {
        store.record_rejection(&ws("w"), host).unwrap();
        clock.advance(1);
    }
    let hosts: Vec<_> = store
        .learned_history(&ws("w"))
        .unwrap()
        .into_iter()
        .map(|r| r.host)
        .collect();
    assert_eq!(hosts, ["c.example", "b.example"]);
    store.record_rejection(&ws("other"), "a.example").unwrap();
    assert_eq!(store.learned_history(&ws("other")).unwrap().len(), 1);
}

#[test]
fn dropping_a_host_removes_only_its_row_and_a_deleted_workspace_loses_all_of_them() {
    let (_clock, store) = store();
    store.record_rejection(&ws("a"), "x.example").unwrap();
    store.record_rejection(&ws("a"), "y.example").unwrap();
    assert!(store.drop_learned_host(&ws("a"), "x.example").unwrap());
    assert!(!store.drop_learned_host(&ws("a"), "x.example").unwrap());
    assert_eq!(store.learned_history(&ws("a")).unwrap().len(), 1);
    let mut conn = lock(&store.conn);
    let tx = conn.transaction().unwrap();
    delete_workspace_rows(&tx, &ws("a")).unwrap();
    tx.commit().unwrap();
    drop(conn);
    assert_eq!(store.learned_history(&ws("a")).unwrap(), []);
}
