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
    /// How many workspaces run now (a restore stops them).
    pub running_workspaces: u32,
}
