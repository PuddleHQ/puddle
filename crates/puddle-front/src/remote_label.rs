// SPDX-License-Identifier: AGPL-3.0-or-later
//! Host labels on a remote host: the editor of a workspace on `<id>.<base>` and one forwarded guest
//! port on `<port>-<id>.<base>`, one label deep so one wildcard certificate covers both. The name is
//! never in a label, so a rename changes no address.
//!
//! A label is read as an exact id first (the editor), else as digits, `-` and an id (a port, with
//! the port rules of the local `<port>.<id>.localhost` origins). A generated id has no hyphen, so
//! only two legacy ids, `<other id>` and `<port>-<other id>`, can make one label mean both: the
//! editor wins and [`shadow_problems`] raises the problem that names both workspaces.

use std::collections::BTreeSet;

use puddle_types::{Problem, Problems, WorkspaceId};

use crate::host_header::{Refusal, Route, guest_port};

/// The key of the problem for the workspace whose editor label also reads as a port of another.
const SHADOW_KEY_PREFIX: &str = "host-label-shadow:";

/// What `host` addresses on a remote host that serves `<label>.<base>`, among the workspaces whose
/// ids are `known`. `host` has no port; `base` has no leading dot. Both are compared in lowercase.
///
/// # Errors
///
/// A [`Refusal`]: `Missing` for no host, `Foreign` for a host that is not exactly one label under
/// `base`, `NotAWorkspace` for a label that is neither a known id nor `<port>-<known id>`.
pub fn route_of_label(
    host: Option<&str>,
    base: &str,
    known: &BTreeSet<WorkspaceId>,
) -> Result<Route, Refusal> {
    let host = host.ok_or(Refusal::Missing)?.to_ascii_lowercase();
    let base = base.to_ascii_lowercase();
    let label = host
        .strip_suffix(base.as_str())
        .and_then(|rest| rest.strip_suffix('.'))
        .filter(|label| !label.is_empty() && !label.contains('.'))
        .ok_or(Refusal::Foreign)?;
    resolve(label, known).ok_or(Refusal::NotAWorkspace)
}

/// The workspace and port a label stands for: an exact id first, else `<port>-<id>`.
fn resolve(label: &str, known: &BTreeSet<WorkspaceId>) -> Option<Route> {
    if let Some(workspace) = known.iter().find(|id| id.as_str() == label) {
        return Some(Route {
            workspace: workspace.clone(),
            guest_port: None,
        });
    }
    resolve_port(label, known)
}

/// Two legacy workspaces whose labels clash: `editor`'s id is `<port>-<id of port_of>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shadow<'a> {
    /// The workspace whose editor the label opens (the winner).
    pub editor: &'a (WorkspaceId, String),
    /// The workspace whose port the label would have been.
    pub port_of: &'a (WorkspaceId, String),
    /// That port.
    pub guest_port: u16,
}

/// Every pair of `workspaces` (id and display name) whose labels clash, ordered by the id of the
/// editor's workspace.
#[must_use]
pub fn shadows(workspaces: &[(WorkspaceId, String)]) -> Vec<Shadow<'_>> {
    let ids: BTreeSet<WorkspaceId> = workspaces.iter().map(|(id, _)| id.clone()).collect();
    let mut found: Vec<Shadow<'_>> = workspaces
        .iter()
        .filter_map(|editor| {
            let route = resolve_port(editor.0.as_str(), &ids)?;
            let port_of = workspaces.iter().find(|(id, _)| *id == route.workspace)?;
            Some(Shadow {
                editor,
                port_of,
                guest_port: route.guest_port?,
            })
        })
        .collect();
    found.sort_by(|a, b| a.editor.0.cmp(&b.editor.0));
    found
}

/// `label` read as `<port>-<id>` only, skipping the exact-id rule.
fn resolve_port(label: &str, known: &BTreeSet<WorkspaceId>) -> Option<Route> {
    let (port, id) = label.split_once('-')?;
    let guest_port = Some(guest_port(port)?);
    let workspace = known.iter().find(|known| known.as_str() == id)?;
    Some(Route {
        workspace: workspace.clone(),
        guest_port,
    })
}

/// The problem for each clash in `workspaces`, keyed `host-label-shadow:<editor's id>` and about
/// the workspace whose editor wins.
#[must_use]
pub fn shadow_problems(workspaces: &[(WorkspaceId, String)]) -> Vec<Problem> {
    shadows(workspaces)
        .into_iter()
        .map(|shadow| {
            let (editor_id, editor_name) = shadow.editor;
            let (_, port_name) = shadow.port_of;
            let port = shadow.guest_port;
            Problem::new(
                format!("{SHADOW_KEY_PREFIX}{editor_id}"),
                format!("{editor_name} and {port_name} share an address on this host"),
                format!(
                    "The address {editor_id}.<host> opens the editor of {editor_name}, and it \
                     would also be port {port} of {port_name}. The editor wins. Port {port} of \
                     {port_name} is still reachable through its editor's /proxy/{port}/ links."
                ),
            )
            .about(editor_id.clone())
        })
        .collect()
}

/// Brings the `host-label-shadow:` problems in `problems` in line with `workspaces`: raises the
/// clashes that are new or whose names changed and clears the ones that are gone. Nothing changes,
/// and nothing is logged, when they already match.
pub fn sync_shadow_problems(problems: &Problems, workspaces: &[(WorkspaceId, String)]) {
    let wanted = shadow_problems(workspaces);
    let open = problems.list();
    for stale in open
        .iter()
        .filter(|p| p.key.starts_with(SHADOW_KEY_PREFIX) && !wanted.iter().any(|w| w.key == p.key))
    {
        problems.clear(&stale.key);
    }
    for problem in wanted {
        if !open.contains(&problem) {
            problems.raise(problem);
        }
    }
}

#[cfg(test)]
mod tests;
