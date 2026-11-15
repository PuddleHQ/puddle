// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

fn ws(name: &str) -> WorkspaceId {
    WorkspaceId::new(name).unwrap()
}

#[test]
fn a_build_without_a_host_has_nothing_learned() {
    let none = NoLearned;
    assert_eq!(none.list(&ws("a")).len(), 0);
    assert!(!none.forget(&ws("a"), "x.example"));
    assert_eq!(none.clear(&ws("a")), 0);
    assert!(!none.discard(&ws("a"), "x.example"));
}

#[test]
fn the_fake_lists_the_passed_through_hosts_soonest_first_then_the_rest_newest_first() {
    let fake = FakeLearned::new();
    fake.learn(&ws("a"), "late.example", 90);
    fake.learn(&ws("a"), "soon.example", 10);
    fake.learn_with_history(&ws("a"), "old.example", None, 2, 5_000);
    fake.learn_with_history(&ws("a"), "newer.example", None, 1, 9_000);
    fake.learn(&ws("b"), "soon.example", 10);
    let hosts: Vec<_> = fake.list(&ws("a")).into_iter().map(|h| h.host).collect();
    assert_eq!(
        hosts,
        [
            "soon.example",
            "late.example",
            "newer.example",
            "old.example"
        ]
    );
    let old = &fake.list(&ws("a"))[3];
    assert_eq!(
        (
            old.passed_through,
            old.expires_in_secs,
            old.first_rejected_at,
            old.last_rejected_at,
            old.count
        ),
        (false, None, 4_000, 5_000, 2)
    );
}

#[test]
fn the_fake_forgets_keeps_the_history_clears_and_discards_by_workspace() {
    let fake = FakeLearned::new();
    fake.learn(&ws("a"), "soon.example", 10);
    fake.learn(&ws("a"), "late.example", 90);
    fake.learn(&ws("b"), "soon.example", 10);
    assert!(!fake.forget(&ws("c"), "soon.example"));
    assert!(fake.forget(&ws("a"), "soon.example"));
    assert!(
        !fake.forget(&ws("a"), "soon.example"),
        "no longer passed through"
    );
    assert_eq!(fake.list(&ws("a")).len(), 2, "the history stays");
    assert_eq!(
        fake.list(&ws("b"))[0].expires_in_secs,
        Some(10),
        "another workspace is untouched"
    );
    assert_eq!(fake.clear(&ws("a")), 1);
    assert_eq!(fake.clear(&ws("a")), 0);
    assert!(fake.discard(&ws("a"), "soon.example"));
    assert!(!fake.discard(&ws("a"), "soon.example"));
    assert!(!fake.discard(&ws("c"), "soon.example"));
    assert_eq!(fake.list(&ws("a")).len(), 1);
}
