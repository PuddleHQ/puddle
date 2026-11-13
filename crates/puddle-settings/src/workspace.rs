// SPDX-License-Identifier: GPL-3.0-or-later
//! The per-workspace settings document: one per workspace, holding its overrides.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::document::{self, Document};
use crate::migrate::{self, Migration};
use crate::{Loaded, SettingsError, WorkspaceLayer};

/// The current shape of [`WorkspaceSettings`] documents.
pub const WORKSPACE_SCHEMA_VERSION: u32 = 1;

/// One workspace's settings. The workspace it belongs to is the store's key, not part of the
/// document, so renaming or importing a workspace doesn't rewrite it.
///
/// Settings that exist only per workspace (never as a global default, like "dangerous"
/// settings) go next to [`WorkspaceSettings::overrides`].
///
/// ```
/// use puddle_settings::WorkspaceSettings;
/// use serde_json::json;
///
/// let s = WorkspaceSettings::from_document(json!({"overrides": {"zoom_hotkeys": false}}))
///     .unwrap()
///     .settings;
/// assert_eq!(s.overrides.zoom_hotkeys, Some(false));
/// assert_eq!(s.to_document().unwrap(), json!({"schema_version": 1, "overrides": {"zoom_hotkeys": false}}));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct WorkspaceSettings {
    /// This workspace's overrides of the global defaults; unset ones inherit.
    #[serde(default, skip_serializing_if = "WorkspaceLayer::is_empty")]
    pub overrides: WorkspaceLayer,
    /// This workspace's tool switches. They exist only per workspace; the global switch
    /// ([`crate::Tools::mise`]) wins over them ([`crate::mise_mode`]).
    #[serde(default, skip_serializing_if = "WorkspaceTools::is_empty")]
    pub tools: WorkspaceTools,
    #[serde(flatten)]
    pub(crate) extra: BTreeMap<String, Value>,
}

/// The per-workspace tool switches; unset means Puddle's default (on).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct WorkspaceTools {
    /// Manage this workspace's tools with mise; default on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mise: Option<bool>,
    /// Let mise run the repository's own code (hooks, `[env]` templates, tasks) in this workspace,
    /// that is trust its `mise.toml`; default on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_code: Option<bool>,
    #[serde(flatten)]
    pub(crate) extra: BTreeMap<String, Value>,
}

impl WorkspaceTools {
    /// Puddle's default for [`WorkspaceTools::mise`].
    pub const DEFAULT_MISE: bool = true;
    /// Puddle's default for [`WorkspaceTools::repo_code`].
    pub const DEFAULT_REPO_CODE: bool = true;

    /// Whether mise manages this workspace, before the global switch is asked.
    #[must_use]
    pub fn mise(&self) -> bool {
        self.mise.unwrap_or(Self::DEFAULT_MISE)
    }

    /// Whether mise may run the repository's code, before the global switch is asked.
    #[must_use]
    pub fn repo_code(&self) -> bool {
        self.repo_code.unwrap_or(Self::DEFAULT_REPO_CODE)
    }

    pub(crate) fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// What the managed mise layer does in one workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MiseMode {
    /// No managed mise: the global switch is off, or this workspace's is.
    Off,
    /// Managed mise; `trust` says whether the repository's own code may run.
    On {
        /// Trust the workspace's `mise.toml` (the `repo_code` switch).
        trust: bool,
    },
}

/// The mise mode of a workspace. The global switch decides first and cannot be overridden: with
/// it off the answer is [`MiseMode::Off`] whatever the workspace stores.
///
/// ```
/// use puddle_settings::{GlobalSettings, MiseMode, WorkspaceSettings, mise_mode};
/// let mut ws = WorkspaceSettings::default();
/// ws.tools.mise = Some(true);
/// let mut global = GlobalSettings::default();
/// assert_eq!(mise_mode(&global, Some(&ws)), MiseMode::On { trust: true });
/// global.tools.mise = Some(false);
/// assert_eq!(mise_mode(&global, Some(&ws)), MiseMode::Off);
/// ```
#[must_use]
pub fn mise_mode(
    global: &crate::GlobalSettings,
    workspace: Option<&WorkspaceSettings>,
) -> MiseMode {
    if !global.tools.mise() {
        return MiseMode::Off;
    }
    let own = workspace.map(|w| &w.tools);
    if !own.is_none_or(WorkspaceTools::mise) {
        return MiseMode::Off;
    }
    MiseMode::On {
        trust: own.is_none_or(WorkspaceTools::repo_code),
    }
}

impl WorkspaceSettings {
    /// Reads a stored document: checks its version, migrates it, keeps unknown fields.
    ///
    /// # Errors
    ///
    /// When the document isn't an object, has a bad or newer `schema_version`, a migration
    /// fails, or a value is invalid. See the crate docs for the rules.
    pub fn from_document(doc: Value) -> Result<Loaded<Self>, SettingsError> {
        document::load(doc)
    }

    /// The document to store: the current `schema_version`, set values only, unknown fields
    /// as they were read.
    ///
    /// # Errors
    ///
    /// [`SettingsError::Unstorable`] when the settings cannot be serialised, which their types
    /// (string keys only) rule out.
    pub fn to_document(&self) -> Result<Value, SettingsError> {
        document::store(self)
    }
}

impl Document for WorkspaceSettings {
    const KIND: &'static str = "workspace";
    const VERSION: u32 = WORKSPACE_SCHEMA_VERSION;
    const MIGRATIONS: &'static [Migration] = migrate::WORKSPACE;

    fn collect_unknown(&self, out: &mut Vec<String>) {
        document::push_unknown("", &self.extra, out);
        self.overrides.collect_unknown("overrides.", out);
        document::push_unknown("tools.", &self.tools.extra, out);
    }
}

#[cfg(test)]
mod tests;
