// SPDX-License-Identifier: GPL-3.0-or-later
//! The warning for a workspace that has Kubernetes tools: the guest kernel cannot route Services.

use puddle_types::{Problem, WorkspaceName};

/// The key of the problem about Kubernetes tools in `workspace`.
pub(crate) fn kubernetes_key(workspace: &WorkspaceName) -> String {
    format!("kubernetes:{workspace}")
}

/// The problem for the Kubernetes `tools` the boot hook found in `workspace`; `None` when it found
/// none.
pub(crate) fn kubernetes_problem(workspace: &WorkspaceName, tools: &[&str]) -> Option<Problem> {
    if tools.is_empty() {
        return None;
    }
    Some(
        Problem::new(
            kubernetes_key(workspace),
            format!("Kubernetes Services do not work in {workspace}"),
            format!(
                "{} is installed in {workspace}. A cluster started with it comes up, but its \
                 Services do not route: the workspace's kernel lacks the netfilter and IPVS \
                 parts Kubernetes needs for them. Reach a pod by its own address or through a \
                 port forward instead of a Service. docs/kubernetes.md in the Puddle repository \
                 has the details; the next start checks again.",
                tools.join(", ")
            ),
        )
        .about(workspace.clone()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace() -> WorkspaceName {
        WorkspaceName::new("acme").unwrap()
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
            problem.detail.contains("port forward")
                && problem.detail.contains("docs/kubernetes.md"),
            "{}",
            problem.detail
        );
    }
}
