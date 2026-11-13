// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use crate::CollectingSink;

fn problems() -> (Arc<CollectingSink>, Problems) {
    let sink = Arc::new(CollectingSink::default());
    (sink.clone(), Problems::new(sink))
}

#[test]
fn a_problem_is_listed_until_it_is_cleared_and_each_change_is_announced_once() {
    let (sink, problems) = problems();
    let first = Problem::new("sweep", "The sweep failed", "It tries again in a minute.");
    problems.raise(first.clone());
    problems.raise(first.clone());
    assert_eq!(problems.list(), [first]);
    assert_eq!(sink.take(), [Event::ProblemsChanged {}]);
    problems.raise(Problem::new("sweep", "The sweep failed", "Disk full."));
    assert_eq!(sink.take(), [Event::ProblemsChanged {}]);
    problems.clear("sweep");
    problems.clear("sweep");
    assert_eq!(problems.list().len(), 0);
    assert_eq!(sink.take(), [Event::ProblemsChanged {}]);
}

#[test]
fn clearing_by_prefix_ends_only_the_matching_problems() {
    let (_, problems) = problems();
    let box_name = WorkspaceId::new("box").unwrap();
    problems.raise(Problem::new("git-locks:box", "a", "b").about(box_name.clone()));
    problems.raise(Problem::new("git-locks:other", "a", "b"));
    problems.clear_prefix("git-locks:box");
    let keys: Vec<_> = problems.list().into_iter().map(|p| p.key).collect();
    assert_eq!(keys, ["git-locks:other"]);
    assert!(format!("{problems:?}").starts_with("Problems"));
}
