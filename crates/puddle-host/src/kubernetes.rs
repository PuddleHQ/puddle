// SPDX-License-Identifier: AGPL-3.0-or-later
//! The warning for a workspace that has Kubernetes tools: the guest kernel cannot route Services.

use puddle_types::{Problem, WorkspaceId};

/// The key of the problem about Kubernetes tools in `workspace`.
pub(crate) fn kubernetes_key(workspace: &WorkspaceId) -> String {
    format!("kubernetes:{workspace}")
}

/// The problem for the Kubernetes `tools` the boot hook found in `workspace`; `None` when it found
/// none.
pub(crate) fn kubernetes_problem(
    workspace: &WorkspaceId,
    name: &str,
    tools: &[&str],
) -> Option<Problem> {
    if tools.is_empty() {
        return None;
    }
    Some(
        Problem::new(
            kubernetes_key(workspace),
            format!("Kubernetes Services do not work in {name}"),
            format!(
                "{} is installed in {name}. A cluster started with it comes up, but its \
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
mod tests;
