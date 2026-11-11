// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;

fn workspace() -> WorkspaceId {
    WorkspaceId::new("acme").unwrap()
}

#[test]
fn no_tools_is_no_problem() {
    assert_eq!(kubernetes_problem(&workspace(), &[]), None);
}

#[test]
fn the_problem_names_the_tools_and_what_to_do() {
    let problem = kubernetes_problem(&workspace(), &["kind", "k3d"]).unwrap();
    assert_eq!(problem.key, "kubernetes:acme");
    assert_eq!(problem.workspace, Some(workspace()));
    assert_eq!(problem.title, "Kubernetes Services do not work in acme");
    assert!(
        problem
            .detail
            .starts_with("kind, k3d is installed in acme."),
        "{}",
        problem.detail
    );
    assert!(
        problem.detail.contains("port forward") && problem.detail.contains("docs/kubernetes.md"),
        "{}",
        problem.detail
    );
}
