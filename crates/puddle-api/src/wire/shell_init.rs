// SPDX-License-Identifier: GPL-3.0-or-later
//! A workspace's shell init on the wire: the script its login shells source. The host's copy is
//! the only source of truth; the workspace sees a read-only copy.

use puddle_types::WorkspaceName;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// The shell init saved for a workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ShellInitView {
    /// The workspace.
    pub workspace: WorkspaceName,
    /// The script; empty when none is saved.
    pub content: String,
    /// Whether one is saved. A workspace with none starts all the same.
    pub saved: bool,
    /// When it was last saved, epoch ms; `null` when none is saved.
    #[schema(required = true)]
    pub changed_at: Option<u64>,
}

/// The script to save.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ShellInitRequest {
    /// The whole script. Line ends become LF. At most 48 KiB, no NUL, not empty (to remove the
    /// shell init, delete it).
    pub content: String,
}

/// What a save did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ShellInitSaved {
    /// What is saved now.
    pub shell_init: ShellInitView,
    /// Whether the script was run in a throwaway shell in the running workspace and printed
    /// nothing. `false` when the workspace is not running: puddle checks it when it starts, and
    /// says so as a problem if it fails then.
    pub checked: bool,
    /// Why it was not checked, when `checked` is `false`.
    #[schema(required = true)]
    pub unchecked_reason: Option<String>,
}
