// SPDX-License-Identifier: AGPL-3.0-or-later
//! How the store decides a request and what it logs when it does, with the database working.

use puddle_types::{PatternKind, RuleSetId};

use super::test_fixtures::{new_pending, request, store, store_with, workspace_rule};
use super::*;
use crate::test_support::Logs;

#[test]
fn the_open_row_cap_is_the_one_the_store_was_opened_with() {
    let (_, store) = store_with(Limits {
        max_open_rows: 7,
        ..Limits::default()
    });
    assert_eq!(store.max_open_rows(), 7);
}

#[test]
fn a_rule_that_arrived_while_a_request_waited_for_the_database_decides_it() {
    let (_, waiting) = store_with(Limits::default());
    let (_, committed) = store_with(Limits::default());
    let rule = committed
        .as_user(UserId::OWNER)
        .add_rule(&workspace_rule("x.example"))
        .unwrap();
    // The snapshot the waiting request finds once it has the lock.
    *waiting.rules.write().unwrap() = committed.snapshot();

    let decision = waiting
        .decide_locked(
            &mut lock(&waiting.conn),
            &request("x.example"),
            1_000_000,
            SuffixAllows::Count,
        )
        .unwrap();

    assert_eq!(
        decision,
        Decision::Allow {
            rule_id: rule.id,
            pattern: PatternKind::Exact
        }
    );
    assert!(
        waiting
            .open_pending_every_user(&crate::testing::all_users(), None)
            .unwrap()
            .is_empty(),
        "no pending row was made"
    );
}

#[test]
fn a_decision_is_logged_with_its_effect() {
    let store = store();
    let id = new_pending(&store, "x.example");
    let logs = Logs::capture();

    let decided = store
        .as_user(UserId::OWNER)
        .resolve_pending(id, &Resolution::allow(), Actor::Ui)
        .unwrap();

    assert_eq!(
        logs.lines(),
        [format!(
            "INFO pending request decided pending={id} workspace=box host=x.example rule={} effect=\"allow\"",
            decided.rule.id
        )]
    );
}

#[test]
fn a_rule_in_a_set_that_is_off_leaves_the_open_requests_it_matches_open() {
    let store = store();
    let created = store
        .create_rule_set(UserId::OWNER, "Blocks", "", None, Actor::Ui)
        .unwrap();
    let set: i64 = created.id.to_string()[5..].parse().unwrap();
    store
        .switch_rule_set(
            UserId::OWNER,
            RuleSetId::User(set),
            None,
            Some(false),
            Actor::Ui,
        )
        .unwrap();
    let id = new_pending(&store, "x.example");

    store
        .as_user(UserId::OWNER)
        .add_rule(&NewRule {
            scope: Scope::Set(set),
            ..workspace_rule("x.example")
        })
        .unwrap();

    assert_eq!(store.pending(id).unwrap().state, PendingState::Requested);
}
