// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

fn ws(name: &str) -> WorkspaceId {
    WorkspaceId::new(name).unwrap()
}

fn own(name: &str) -> OwnedWorkspace {
    OwnedWorkspace::for_test(puddle_store::UserId::OWNER, &ws(name))
}

#[test]
fn a_build_without_a_host_has_nothing_learned() {
    let none = NoLearned;
    assert_eq!(none.list(&own("a")).len(), 0);
    assert!(!none.forget(&own("a"), "x.example"));
    assert_eq!(none.clear(&own("a")), 0);
    assert!(!none.discard(&own("a"), "x.example"));
}

#[test]
fn the_fake_lists_the_passed_through_hosts_soonest_first_then_the_rest_newest_first() {
    let fake = FakeLearned::new();
    fake.learn(&ws("a"), "late.example", 90);
    fake.learn(&ws("a"), "soon.example", 10);
    fake.learn_with_history(&ws("a"), "old.example", None, 2, 5_000);
    fake.learn_with_history(&ws("a"), "newer.example", None, 1, 9_000);
    fake.learn(&ws("b"), "soon.example", 10);
    let hosts: Vec<_> = fake.list(&own("a")).into_iter().map(|h| h.host).collect();
    assert_eq!(
        hosts,
        [
            "soon.example",
            "late.example",
            "newer.example",
            "old.example"
        ]
    );
    let old = &fake.list(&own("a"))[3];
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
    assert!(!fake.forget(&own("c"), "soon.example"));
    assert!(fake.forget(&own("a"), "soon.example"));
    assert!(
        !fake.forget(&own("a"), "soon.example"),
        "no longer passed through"
    );
    assert_eq!(fake.list(&own("a")).len(), 2, "the history stays");
    assert_eq!(
        fake.list(&own("b"))[0].expires_in_secs,
        Some(10),
        "another workspace is untouched"
    );
    assert_eq!(fake.clear(&own("a")), 1);
    assert_eq!(fake.clear(&own("a")), 0);
    assert!(fake.discard(&own("a"), "soon.example"));
    assert!(!fake.discard(&own("a"), "soon.example"));
    assert!(!fake.discard(&own("c"), "soon.example"));
    assert_eq!(fake.list(&own("a")).len(), 1);
}
