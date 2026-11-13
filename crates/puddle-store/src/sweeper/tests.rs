// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;
use crate::clock::ManualClock;
use crate::{Actor, Effect, Limits, NewRule, Pattern, Scope, UserId};
use puddle_test_kit::eventually;

/// A rule that has expired, so the next pass has to write a `rule_expired` record.
fn store_with_an_expired_rule() -> Arc<Store> {
    let clock = Arc::new(ManualClock::new(1_000_000));
    let store = Arc::new(Store::open_in_memory(clock.clone(), Limits::default()).unwrap());
    store
        .as_user(UserId::OWNER)
        .add_rule(&NewRule {
            scope: Scope::Global,
            pattern: Pattern::parse("old.example").unwrap(),
            effect: Effect::Allow,
            expires_at: Some(1_000_100),
            created_by: Actor::Cli,
        })
        .unwrap();
    clock.advance(1_000);
    store
}

#[tokio::test]
async fn a_sweep_that_cannot_write_is_a_problem_until_one_works() {
    let store = store_with_an_expired_rule();
    store.fail_audit_writes("rule_expired");
    let problems = Arc::new(Problems::new(Arc::new(NullSink)));
    let sweeper =
        Sweeper::spawn_reporting(store.clone(), Duration::from_millis(20), problems.clone());
    eventually("the sweep problem", Duration::from_secs(5), || {
        !problems.list().is_empty()
    })
    .await;
    let problem = problems.list().remove(0);
    assert_eq!(problem.key, SWEEP_PROBLEM);
    assert!(problem.detail.contains("disk full"), "{problem:?}");
    assert!(problem.detail.contains("free some space"), "{problem:?}");
    store.audit_writes_work_again();
    eventually("the problem to end", Duration::from_secs(5), || {
        problems.list().is_empty()
    })
    .await;
    sweeper.shutdown().await;
    assert_eq!(
        store.rules_every_user().len(),
        0,
        "the rule was removed in the end"
    );
}

#[tokio::test]
async fn a_sweeper_whose_task_was_cancelled_says_so_when_shut_down() {
    let store = store_with_an_expired_rule();
    let logs = crate::test_support::Logs::capture();
    let sweeper = Sweeper::spawn(store, Duration::from_secs(60));
    sweeper.task.abort();
    sweeper.shutdown().await;
    let lines = logs.lines();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(
        lines[0].starts_with("WARN sweeper ended abnormally err=")
            && lines[0].ends_with("was cancelled"),
        "{lines:?}"
    );
}
