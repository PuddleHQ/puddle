// SPDX-License-Identifier: GPL-3.0-or-later
use puddle_types::{PatternKind, RuleSetId};

use super::*;

#[test]
fn a_failure_says_the_last_line_the_agent_wrote() {
    let out = ExecOutput::new(1, "", "warning: slow\n\ncould not download mise: 403\n\n");
    assert_eq!(failure_reason(Ok(out)), "could not download mise: 403");
}

#[test]
fn a_failure_without_a_message_says_the_exit_code() {
    assert_eq!(
        failure_reason(Ok(ExecOutput::new(137, "", "  \n"))),
        "the download ended with exit code 137"
    );
}

#[test]
fn a_command_that_could_not_run_says_why() {
    let err = ComputeError::NotFound {
        sandbox: "acme".to_owned(),
    };
    assert_eq!(failure_reason(Err(err)), "sandbox \"acme\" not found");
}

#[test]
fn only_a_deny_is_named_as_the_blocker() {
    let pattern = |id: RuleId| (id == RuleId(7)).then(|| "mise.jdx.dev".to_owned());
    let deny = |id| Decision::Deny {
        rule_id: RuleId(id),
        pattern: PatternKind::Exact,
    };
    assert_eq!(
        blocker_words(deny(7), pattern).unwrap(),
        "A rule you made blocks mise.jdx.dev: deny mise.jdx.dev. Remove it on the Network tab to allow the download."
    );
    // A rule that vanished between the decision and the lookup names nothing.
    assert_eq!(blocker_words(deny(8), pattern), None);
    let set = Decision::SetDeny {
        set: RuleSetId::User(3),
        rule_id: None,
        pattern: PatternKind::Suffix,
    };
    assert!(
        blocker_words(set, pattern)
            .unwrap()
            .starts_with("A rule set you switched on blocks mise.jdx.dev")
    );
    let allow = Decision::Allow {
        rule_id: RuleId(7),
        pattern: PatternKind::Exact,
    };
    assert_eq!(blocker_words(allow, pattern), None);
}

fn ws(name: &str) -> WorkspaceId {
    WorkspaceId::new(name).unwrap()
}

#[test]
fn an_allowed_request_the_install_waited_on_runs_it_once_and_a_denied_one_never() {
    let runs = ToolRuns::default();
    runs.wait_on(&ws("acme"), BTreeSet::from([4, 5]));
    assert!(!runs.closed(&ws("acme"), 9, true), "not waited on");
    assert!(!runs.closed(&ws("other"), 4, true), "another workspace");
    assert!(!runs.closed(&ws("acme"), 4, false), "denied");
    assert!(runs.closed(&ws("acme"), 5, true), "allowed: one more run");
    assert!(!runs.closed(&ws("acme"), 5, true), "only once");
    runs.wait_on(&ws("acme"), BTreeSet::from([6, 7]));
    assert!(runs.closed(&ws("acme"), 6, true));
    assert!(
        !runs.closed(&ws("acme"), 7, true),
        "the run an allow starts looks at every host again"
    );
    runs.wait_on(&ws("acme"), BTreeSet::from([8]));
    runs.wait_on(&ws("acme"), BTreeSet::new());
    assert!(
        !runs.closed(&ws("acme"), 8, true),
        "a good install waits on nothing"
    );
}

#[tokio::test]
async fn one_lock_per_workspace_and_an_unused_one_is_dropped() {
    let runs = ToolRuns::default();
    let a = runs.lock_of(&ws("acme"));
    let held = a.lock().await;
    assert!(
        runs.lock_of(&ws("acme")).try_lock().is_err(),
        "the same lock"
    );
    assert!(runs.lock_of(&ws("other")).try_lock().is_ok());
    drop(held);
    drop(a);
    let _b = runs.lock_of(&ws("third"));
    let names: Vec<String> = runs
        .locks
        .lock()
        .unwrap()
        .keys()
        .map(ToString::to_string)
        .collect();
    assert_eq!(names, ["third"], "locks nobody holds are gone");
}
