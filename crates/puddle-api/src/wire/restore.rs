// SPDX-License-Identifier: AGPL-3.0-or-later
//! Restore from a backup file, on the wire.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// A part of a backup that the preview counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RestorePartName {
    /// Allow and deny rules and request rules.
    Rules,
    /// Rule sets.
    RuleSets,
    /// The workspace list.
    Workspaces,
    /// Git identities.
    Identities,
    /// Environment variables.
    Environment,
    /// The settings documents, by key.
    Settings,
    /// The users of the backup.
    Users,
}

/// How one part of the backup compares with this machine (entries match by natural key).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RestorePart {
    /// Which part.
    pub name: RestorePartName,
    /// Entries in the backup.
    pub in_backup: u32,
    /// In the backup and not here: both modes add them.
    pub add: u32,
    /// In both and different: "Add what is missing" keeps yours, "Replace" takes the backup's.
    pub differ: u32,
    /// In both and equal.
    pub same: u32,
    /// Here and not in the backup: "Add what is missing" keeps them, "Replace" drops them.
    pub only_here: u32,
}

/// Whether the disk of a restored workspace is on this machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RestoreDisk {
    /// Linked again by the restore.
    Present,
    /// Not on this machine: listed as "disk missing", not started, recreate offered.
    Missing,
}

/// A workspace the backup lists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RestoreWorkspace {
    /// Its id.
    pub id: String,
    /// What the backup calls it.
    pub name: String,
    /// Whether its disk is here.
    pub disk: RestoreDisk,
}

/// A secret variable that arrives without a value because this machine's store lacks it. Names
/// only, never a value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RestoreSecret {
    /// The workspace it belongs to; none for a global variable.
    #[schema(required = true)]
    pub workspace_id: Option<String>,
    /// The variable's name.
    pub name: String,
}

/// A Git identity's pasted token that arrives without its value because this machine's store lacks
/// it. The identity and the host only, never a value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RestoreIdentityToken {
    /// The identity's label.
    pub identity: String,
    /// The Git host the token is for.
    pub host: String,
}

/// What a restore of the uploaded file would do. Choosing a file changes nothing (BK-10).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RestorePreview {
    /// Names the staged copy for the apply step. A copy that is not applied is removed at the next
    /// start and after 24 hours.
    pub staging_id: String,
    /// The backup format (1).
    pub format: u32,
    /// The Puddle that made the backup; none for a backup without a manifest.
    #[schema(required = true)]
    pub puddle_version: Option<String>,
    /// The schema version of the backup's database (before it is read forward).
    pub schema_version: u32,
    /// When it was made, in milliseconds since the Unix epoch; none without a manifest.
    #[schema(required = true)]
    pub created_at_ms: Option<u64>,
    /// How each part compares with this machine.
    pub parts: Vec<RestorePart>,
    /// The workspaces the backup lists, with whether their disk is here.
    pub workspaces: Vec<RestoreWorkspace>,
    /// The secrets that need a value here.
    pub secrets_needing_value: Vec<RestoreSecret>,
    /// The Git identities' pasted tokens that need to be entered again here.
    pub identity_tokens_needing_value: Vec<RestoreIdentityToken>,
    /// How many workspaces run now (a restore stops them).
    pub running_workspaces: u32,
}

/// How a restore treats what is already on this machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RestoreMode {
    /// "Add what is missing": nothing present is changed or removed.
    Add,
    /// "Replace": rules, identities, environment rows, settings and the workspace list become the
    /// backup's. Users, their passwords and devices stay.
    Replace,
}

/// `POST /api/restore/apply`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RestoreApplyRequest {
    /// The staged copy a preview named.
    pub staging_id: String,
    /// The mode the user confirmed.
    pub mode: RestoreMode,
}

/// A restore that is confirmed and waits for the next start.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RestoreApplied {
    /// The safety copy of the present state, a folder name in the safety copies list.
    pub safety_copy: String,
    /// Whether Puddle must be started again for the restore to apply. The desktop app restarts
    /// itself; `puddle serve` exits and applies the restore at its next start.
    pub restart_required: bool,
}

/// What the last restore did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RestoreResult {
    /// Whether it was applied; `false` means it was undone and the old state is in use.
    pub applied: bool,
    /// The mode that was asked for.
    pub mode: RestoreMode,
    /// The safety copy's folder name.
    pub safety_copy: String,
    /// When it finished, in milliseconds since the Unix epoch.
    pub at_ms: u64,
    /// What changed, or why it was undone.
    pub message: String,
}

/// The last restore's result, until the user has seen it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RestoreResultView {
    /// `null` when there is none to show.
    #[schema(required = true)]
    pub result: Option<RestoreResult>,
}

/// One safety copy of the state a restore replaced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SafetyCopy {
    /// The folder name, the UTC time it was made (`20261115T225500Z`).
    pub name: String,
    /// What it takes on disk.
    pub size_bytes: u64,
}

/// The safety copies, newest first. Puddle never removes one; the user does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SafetyCopyList {
    /// The copies.
    pub copies: Vec<SafetyCopy>,
}
