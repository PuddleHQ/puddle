// SPDX-License-Identifier: AGPL-3.0-or-later
//! System managed: which hosts Puddle allows because of the user's own setup (rules spec R-41).
//! Derived from the settings here and handed to the store, at start and after every settings
//! or consent change, so the allows follow the choices that justify them.

use puddle_settings::{GlobalSettings, ServerChoice};
use puddle_store::{SystemPlan, SystemReason};
use puddle_types::{Problem, WorkspaceId};

use puddle_settings::resolve;

use crate::error::blocking;
use crate::routes::AppState;
use crate::routes::settings::{load_global, load_workspace};

/// What direct SSH gives a workspace: each desktop editor's server download and extension hosts.
const DIRECT_SSH_REASONS: [SystemReason; 4] = [
    SystemReason::DirectSsh,
    SystemReason::DirectSshCursor,
    SystemReason::DirectSshJetBrains,
    SystemReason::DirectSshZed,
];

/// The System managed reasons for a setup.
///
/// - The browser editor's server is a global choice: Microsoft's server (only with a consent
///   that covers today's terms) gives Microsoft's download and Marketplace hosts to every workspace; the
///   bundled code-server gives Open VSX.
/// - `direct_ssh`: the workspaces with direct SSH on, which get the hosts of the desktop editors
///   for themselves (VS Code installs Microsoft's server and extensions there; Cursor, `JetBrains` Gateway and Zed, their own).
///
/// Nothing else is derived: a workspace used with another tool gets no host for VS Code's sake.
#[must_use]
pub(crate) fn plan(global: &GlobalSettings, direct_ssh: &[WorkspaceId]) -> SystemPlan {
    let mut plan = SystemPlan::default();
    match global.editor_server() {
        ServerChoice::Microsoft => {
            plan.everywhere.insert(SystemReason::MicrosoftServer);
        }
        // Includes Microsoft's server chosen without a consent that covers today's terms: the
        // editor runs code-server then, so it gets Open VSX and Microsoft's hosts stay closed.
        ServerChoice::CodeServer => {
            plan.everywhere.insert(SystemReason::CodeServer);
        }
    }
    for workspace in direct_ssh {
        plan.workspaces
            .entry(workspace.clone())
            .or_default()
            .extend(DIRECT_SSH_REASONS);
    }
    plan
}

/// Derives the plan from the stored settings and the workspaces, and hands it to the store. Run
/// at start and after every change that can move it: global or workspace settings, a consent, a
/// workspace made or deleted. Takes the settings lock itself, so call it after releasing it.
///
/// A failure leaves the store's reasons as they were and raises the problem [`PROBLEM_KEY`], which
/// the next successful refresh ends. A workspace list that can't be read counts as no workspace
/// with direct SSH on (an allow is only ever dropped then) and raises the same problem.
pub(crate) async fn refresh(state: &AppState) {
    let (names, list_error): (Vec<WorkspaceId>, Option<String>) =
        match state.workspaces.list().await {
            Ok(records) => (records.into_iter().map(|w| w.id).collect(), None),
            Err(err) => (Vec::new(), Some(err.to_string())),
        };
    let (store, settings) = (state.store.clone(), state.settings.clone());
    let _lock = state.settings_lock.lock().await;
    let result = blocking(move || {
        let repo = settings.as_ref();
        let global = load_global(repo)?;
        let direct: Vec<WorkspaceId> = names
            .into_iter()
            .filter(|name| {
                load_workspace(repo, name).is_ok_and(|own| {
                    resolve(&global.settings, Some(&own.settings))
                        .direct_ssh
                        .value
                })
            })
            .collect();
        Ok(store.set_system_managed(&plan(&global.settings, &direct))?)
    })
    .await;
    match (result, list_error) {
        (Ok(closed), None) => {
            state.problems.clear(PROBLEM_KEY);
            if !closed.is_empty() {
                let count = closed.len();
                tracing::info!(count, "System managed hosts decided waiting requests");
            }
        }
        (Ok(_), Some(reason)) => state.problems.raise(Problem::new(
            PROBLEM_KEY,
            "Puddle could not read the workspace list, so it allows no hosts for direct SSH",
            format!(
                "{reason}. Hosts that desktop VS Code needs in workspaces with direct SSH are \
                 blocked until the list can be read; change a setting or restart Puddle to try again."
            ),
        )),
        (Err(err), _) => state.problems.raise(Problem::new(
            PROBLEM_KEY,
            "Puddle could not update which hosts it allows for your setup",
            format!(
                "{}. The hosts it allowed before stay as they were, so a change you just made to \
                 the editor server or direct SSH may not apply yet; change the setting again to \
                 try again.",
                err.reason()
            ),
        )),
    }
}

/// The key of the problem a failed [`refresh`] raises.
const PROBLEM_KEY: &str = "system-managed";

#[cfg(test)]
mod tests;
