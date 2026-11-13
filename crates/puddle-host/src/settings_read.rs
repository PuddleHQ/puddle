// SPDX-License-Identifier: GPL-3.0-or-later
//! Reading the settings documents for the host's own decisions (direct SSH, local destinations,
//! the default memory). A missing document is a fresh install and means the defaults; one that
//! exists but cannot be read or understood is an `Err` naming what is wrong, so each caller picks
//! fail-closed, refuse or report instead of treating damage as "nothing set".

use puddle_api::SettingsRepo;
use puddle_settings::{GlobalSettings, MiseMode, WorkspaceSettings, mise_mode};
use puddle_types::WorkspaceId;
use serde_json::json;

/// The global settings; the defaults when none were ever saved.
pub(crate) fn global(repo: &dyn SettingsRepo) -> Result<GlobalSettings, String> {
    let doc = repo
        .load_global()
        .map_err(|e| format!("the global settings cannot be read: {e}"))?
        .unwrap_or_else(|| json!({}));
    GlobalSettings::from_document(doc)
        .map(|loaded| loaded.settings)
        .map_err(|e| format!("the global settings are not valid: {e}"))
}

/// One workspace's own settings; empty when it has none.
pub(crate) fn workspace(
    repo: &dyn SettingsRepo,
    name: &WorkspaceId,
) -> Result<WorkspaceSettings, String> {
    let doc = repo
        .load_workspace(name)
        .map_err(|e| format!("the settings of workspace {name} cannot be read: {e}"))?
        .unwrap_or_else(|| json!({}));
    WorkspaceSettings::from_document(doc)
        .map(|loaded| loaded.settings)
        .map_err(|e| format!("the settings of workspace {name} are not valid: {e}"))
}

/// What the managed mise layer does in one workspace: [`MiseMode::Off`] while the global switch is
/// off, whatever the workspace stores.
pub(crate) fn mise(repo: &dyn SettingsRepo, name: &WorkspaceId) -> Result<MiseMode, String> {
    let global = global(repo)?;
    if !global.tools.mise() {
        // Off needs no workspace document, so a damaged one cannot turn the layer back on.
        return Ok(MiseMode::Off);
    }
    Ok(mise_mode(&global, Some(&workspace(repo, name)?)))
}

#[cfg(test)]
mod tests;
