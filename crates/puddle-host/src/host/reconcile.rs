// SPDX-License-Identifier: GPL-3.0-or-later
//! Adopting and reconciling the runtime's workspaces at start.

use std::collections::{BTreeMap, BTreeSet};

use puddle_compute::{Runtime, SandboxInfo};
use puddle_lifecycle::{Inventory, adopt_workspaces, reconcile};
use puddle_types::{Problem, WorkspaceId, WorkspaceStatus};
use puddle_workspace::Workspaces;

use crate::workspaces::known_ids;
use crate::{HostConfig, HostError};

/// What reconcile works from: the workspaces Puddle knows, the sandboxes they own and which of
/// those exist.
pub(super) async fn inventory_of<R: Runtime>(
    runtime: &R,
    stored: &[crate::files::Stored],
) -> Result<Inventory, HostError> {
    let existing: BTreeSet<String> = runtime
        .list()
        .await?
        .into_iter()
        .map(|SandboxInfo { name, .. }| name)
        .collect();
    let workspaces = known_ids(stored);
    let mut inventory = Inventory::default();
    for stored in stored.iter().filter(|s| !s.creating) {
        if existing.contains(stored.id.as_str()) {
            inventory
                .attached
                .insert(stored.id.clone(), stored.id.sandbox_name());
        }
        inventory.sandboxes.insert(stored.id.sandbox_name());
    }
    inventory.workspaces = workspaces;
    inventory.interrupted = stored
        .iter()
        .filter(|s| s.creating)
        .map(|s| s.id.clone())
        .collect();
    Ok(inventory)
}

/// The workspaces' holders, rebuilt from what exists in the runtime.
pub(super) async fn adopt_holders<R: Runtime>(
    runtime: &R,
    config: &HostConfig,
    stored: &[crate::files::Stored],
) -> Result<Workspaces, HostError> {
    let workspaces = Workspaces::new(config.workspaces.clone());
    let inventory = inventory_of(runtime, stored).await?;
    let adopted = adopt_workspaces(&workspaces, &inventory);
    tracing::info!(adopted = adopted.len(), "workspace holders rebuilt");
    Ok(workspaces)
}

/// Runs reconcile and returns its report with the state each known sandbox was left in.
pub(super) async fn reconcile_with<R: Runtime>(
    runtime: &R,
    config: &HostConfig,
    stored: &[crate::files::Stored],
) -> Result<
    (
        puddle_lifecycle::ReconcileReport,
        BTreeMap<WorkspaceId, WorkspaceStatus>,
    ),
    HostError,
> {
    let inventory = inventory_of(runtime, stored).await?;
    let report = reconcile(runtime, &inventory, &config.shutdown)
        .await
        .map_err(|e| HostError::Reconcile(e.to_string()))?;
    if !report.unknown_volumes.is_empty() {
        // Kept, never removed: the list may be the thing that is missing or out of date.
        tracing::warn!(
            volumes = ?report.unknown_volumes,
            "workspace volumes that no workspace in the list claims were kept"
        );
    }
    if !report.removed.is_empty() {
        // Only the sandbox's own disk goes (its workspace volume is never touched), and a restart
        // rebuilds a sandbox from the volume anyway; say so rather than remove silently.
        tracing::warn!(
            sandboxes = ?report.removed,
            "sandbox records that no workspace in the list claims were removed (their root disks are discarded; workspace volumes are untouched)"
        );
    }
    for failure in &report.failures {
        tracing::warn!(item = %failure.item, action = failure.action, error = %failure.error, "reconcile did not finish this");
    }
    let mut status: BTreeMap<WorkspaceId, WorkspaceStatus> = report
        .crashed
        .iter()
        .filter_map(|name| Some((name.workspace_id()?, WorkspaceStatus::Crashed)))
        .collect();
    // A workspace whose volume is gone shows that right away, not as stopped until a start
    // fails. It wins over `crashed`: the start would be refused anyway.
    for stored in stored {
        if report.missing_volumes.contains(&stored.id) {
            tracing::warn!(workspace = %stored.id, "a workspace's volume is gone; it shows as volume_missing");
            status.insert(stored.id.clone(), WorkspaceStatus::VolumeMissing);
        }
    }
    Ok((report, status))
}

/// What start-up reconcile found or could not do, as problems for the user: each one names the
/// things and what happened to them. None when it found nothing to say.
pub(super) fn reconcile_problems(report: &puddle_lifecycle::ReconcileReport) -> Vec<Problem> {
    let mut problems = Vec::new();
    let unknown: Vec<WorkspaceId> = report
        .unknown_volumes
        .iter()
        .filter_map(puddle_types::VolumeName::workspace_id)
        .collect();
    problems.extend(crate::workspaces::orphans::problem(&unknown));
    if !report.removed.is_empty() {
        let names: Vec<_> = report.removed.iter().map(ToString::to_string).collect();
        problems.push(Problem::new(
            "reconcile-removed-sandboxes",
            format!(
                "Puddle removed {} sandbox{} that no workspace in its list claims",
                names.len(),
                if names.len() == 1 { "" } else { "es" }
            ),
            format!(
                "{}. Only their own root disks were discarded; workspace volumes are untouched. \
                 A workspace you start again gets a fresh sandbox on its volume.",
                names.join(", ")
            ),
        ));
    }
    if !report.failures.is_empty() {
        let what: Vec<_> = report
            .failures
            .iter()
            .map(|f| format!("{} ({}): {}", f.item, f.action, f.error))
            .collect();
        problems.push(Problem::new(
            "reconcile-failures",
            format!(
                "Puddle could not clean up {} thing{} an earlier run left",
                what.len(),
                if what.len() == 1 { "" } else { "s" }
            ),
            format!(
                "{}. They stay as they are and Puddle tries again at its next start.",
                what.join("; ")
            ),
        ));
    }
    problems
}
