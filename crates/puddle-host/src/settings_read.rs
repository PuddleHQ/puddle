// SPDX-License-Identifier: GPL-3.0-or-later
//! Reading the settings documents for the host's own decisions (direct SSH, local destinations,
//! the default memory). A missing document is a fresh install and means the defaults; one that
//! exists but cannot be read or understood is a [`SettingsReadError`] naming what is wrong, so each caller picks
//! fail-closed, refuse or report instead of treating damage as "nothing set".

use puddle_api::SettingsRepo;
use puddle_settings::{GlobalSettings, MiseMode, WorkspaceSettings, mise_mode};
use puddle_types::WorkspaceId;
use serde_json::json;

/// Why a settings document could not be used.
#[derive(Debug, thiserror::Error)]
pub(crate) enum SettingsReadError {
    /// The global document could not be read from the store.
    #[error("the global settings cannot be read: {0}")]
    GlobalUnreadable(#[source] puddle_api::SettingsRepoError),
    /// The global document is damaged.
    #[error("the global settings are not valid: {0}")]
    GlobalInvalid(#[source] puddle_settings::SettingsError),
    /// A workspace's document could not be read from the store.
    #[error("the settings of workspace {name} cannot be read: {source}")]
    WorkspaceUnreadable {
        /// The workspace.
        name: WorkspaceId,
        /// What went wrong.
        #[source]
        source: puddle_api::SettingsRepoError,
    },
    /// A workspace's document is damaged.
    #[error("the settings of workspace {name} are not valid: {source}")]
    WorkspaceInvalid {
        /// The workspace.
        name: WorkspaceId,
        /// What went wrong.
        #[source]
        source: puddle_settings::SettingsError,
    },
}

/// The global settings; the defaults when none were ever saved.
pub(crate) fn global(repo: &dyn SettingsRepo) -> Result<GlobalSettings, SettingsReadError> {
    let doc = repo
        .load_global()
        .map_err(SettingsReadError::GlobalUnreadable)?
        .unwrap_or_else(|| json!({}));
    GlobalSettings::from_document(doc)
        .map(|loaded| loaded.settings)
        .map_err(SettingsReadError::GlobalInvalid)
}

/// One workspace's own settings; empty when it has none.
pub(crate) fn workspace(
    repo: &dyn SettingsRepo,
    name: &WorkspaceId,
) -> Result<WorkspaceSettings, SettingsReadError> {
    let doc = repo
        .load_workspace(name)
        .map_err(|source| SettingsReadError::WorkspaceUnreadable {
            name: name.clone(),
            source,
        })?
        .unwrap_or_else(|| json!({}));
    WorkspaceSettings::from_document(doc)
        .map(|loaded| loaded.settings)
        .map_err(|source| SettingsReadError::WorkspaceInvalid {
            name: name.clone(),
            source,
        })
}

/// What the managed mise layer does in one workspace: [`MiseMode::Off`] while the global switch is
/// off, whatever the workspace stores. Nothing reads it yet (the layer's pieces follow).
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the tools layer starts reading it in the next tasks"
    )
)]
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
