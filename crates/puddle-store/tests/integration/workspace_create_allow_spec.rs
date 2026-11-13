// SPDX-License-Identifier: GPL-3.0-or-later
//! A workspace create allows the host of its repositories (storage ST-44a), through the store's
//! public API.
#![expect(
    clippy::unwrap_used,
    reason = "test helpers outside #[test] functions fail the test by panicking"
)]

use std::sync::Arc;

use puddle_store::{Actor, Effect, Limits, ManualClock, NewRule, Pattern, Scope, Store};
use puddle_types::{Decision, EgressRequest, Host, PendingOutcome, SuffixAllows, WorkspaceId};

fn store() -> Store {
    Store::open_in_memory(
        Arc::new(ManualClock::new(1_800_000_000_000)),
        Limits::default(),
    )
    .unwrap()
}

fn ws(id: &str) -> WorkspaceId {
    WorkspaceId::new(id).unwrap()
}

fn host(name: &str) -> Host {
    Host::parse_normalised(name).unwrap()
}

fn deny(store: &Store, scope: Scope, pattern: &str) {
    store
        .add_rule(&NewRule {
            scope,
            pattern: Pattern::parse(pattern).unwrap(),
            effect: Effect::Deny,
            expires_at: None,
            created_by: Actor::Ui,
        })
        .unwrap();
}

#[test]
fn a_fresh_store_does_not_allow_github_so_a_clone_waits_for_the_user() {
    // The check behind the create's grant: a repository host is pending until some rule says
    // otherwise, with or without a Git identity (a credential never allows a host).
    let store = store();
    assert_eq!(store.peek(&ws("aaaaaaaa"), &host("github.com")), None);
    let decision = store
        .decide(
            &EgressRequest::new(ws("aaaaaaaa"), host("github.com"), 443),
            SuffixAllows::Count,
        )
        .unwrap();
    assert!(matches!(
        decision,
        Decision::Pending(PendingOutcome::New(_))
    ));
}

#[test]
fn peek_records_nothing() {
    let store = store();
    assert_eq!(store.peek(&ws("aaaaaaaa"), &host("github.com")), None);
    assert_eq!(store.inbox_every_user().unwrap().len(), 0);
}

#[test]
fn the_create_allows_a_new_host_for_that_workspace_only_as_a_rule_it_made() {
    let store = store();
    let made = store
        .allow_hosts_for_create(&ws("aaaaaaaa"), &[host("github.com")])
        .unwrap();
    assert_eq!(made.len(), 1);
    let rule = made.first().unwrap();
    assert_eq!(rule.created_by, Actor::WorkspaceCreate);
    assert_eq!(rule.effect, Effect::Allow);
    assert_eq!(rule.scope, Scope::Workspace(ws("aaaaaaaa")));
    assert_eq!(rule.expires_at, None);
    assert!(store.rules_every_user().iter().any(|r| r.id == rule.id));
    assert!(matches!(
        store.peek(&ws("aaaaaaaa"), &host("github.com")),
        Some(Decision::Allow { .. })
    ));
    // Another workspace is unaffected.
    assert_eq!(store.peek(&ws("bbbbbbbb"), &host("github.com")), None);
    // The audit says who made it.
    let lines = store.audit_lines(0, 100).unwrap();
    assert!(
        lines
            .iter()
            .any(|(_, l)| l.contains("rule_created") && l.contains("workspace_create")),
        "{lines:?}"
    );
}

#[test]
fn a_host_that_is_allowed_already_gets_no_rule_and_a_second_create_adds_none() {
    let store = store();
    store
        .add_rule(&NewRule {
            scope: Scope::Global,
            pattern: Pattern::parse(".github.com").unwrap(),
            effect: Effect::Allow,
            expires_at: None,
            created_by: Actor::Ui,
        })
        .unwrap();
    let made = store
        .allow_hosts_for_create(
            &ws("aaaaaaaa"),
            &[host("api.github.com"), host("gitea.example")],
        )
        .unwrap();
    assert_eq!(made.len(), 1, "only the host nothing allowed yet");
    assert_eq!(made.first().unwrap().pattern.to_string(), "gitea.example");
    let again = store
        .allow_hosts_for_create(&ws("aaaaaaaa"), &[host("gitea.example")])
        .unwrap();
    assert_eq!(again.len(), 0);
}

#[test]
fn a_block_rule_is_never_overridden_by_the_create() {
    let store = store();
    deny(&store, Scope::Global, ".github.com");
    deny(&store, Scope::Workspace(ws("aaaaaaaa")), "gitlab.com");
    let made = store
        .allow_hosts_for_create(
            &ws("aaaaaaaa"),
            &[host("api.github.com"), host("gitlab.com")],
        )
        .unwrap();
    assert_eq!(made.len(), 0);
    assert!(matches!(
        store.peek(&ws("aaaaaaaa"), &host("api.github.com")),
        Some(Decision::Deny { .. })
    ));
}

#[test]
fn the_grant_closes_a_pending_request_it_decides() {
    let store = store();
    let decision = store
        .decide(
            &EgressRequest::new(ws("aaaaaaaa"), host("github.com"), 443),
            SuffixAllows::Count,
        )
        .unwrap();
    assert!(matches!(decision, Decision::Pending(_)));
    assert_eq!(store.inbox_every_user().unwrap().len(), 1);
    store
        .allow_hosts_for_create(&ws("aaaaaaaa"), &[host("github.com")])
        .unwrap();
    assert_eq!(store.inbox_every_user().unwrap().len(), 0);
}
