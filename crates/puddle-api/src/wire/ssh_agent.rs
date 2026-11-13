// SPDX-License-Identifier: GPL-3.0-or-later
//! The filtered SSH agent on the wire: the hosts a workspace may use the user's SSH agent for.

use puddle_types::WorkspaceId;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// One host a workspace may use the user's SSH agent for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SshAgentHostView {
    /// The host, as `ssh` in the workspace names it: a lower-case name or an IP address.
    pub host: String,
    /// The keys it may use, as SHA-256 fingerprints (`SHA256:...`, as `ssh-keygen -l` prints
    /// them), sorted; `null` for every key the agent holds.
    #[schema(required = true)]
    pub keys: Option<Vec<String>>,
    /// Epoch ms the agent was turned on for the host, or its keys last changed.
    pub changed_at: u64,
}

impl From<puddle_store::SshAgentHost> for SshAgentHostView {
    fn from(row: puddle_store::SshAgentHost) -> Self {
        Self {
            host: row.host,
            keys: row.keys,
            changed_at: row.changed_at,
        }
    }
}

/// The hosts a workspace may use the user's SSH agent for. Empty unless the user turned it on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SshAgentView {
    /// The workspace.
    pub workspace: WorkspaceId,
    /// The hosts, sorted by name.
    pub hosts: Vec<SshAgentHostView>,
}

/// Which keys a host may use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SshAgentHostRequest {
    /// SHA-256 fingerprints (`SHA256:` and 43 base64 characters, at most 64 of them); absent or
    /// `null` for every key the agent holds.
    #[serde(default)]
    pub keys: Option<Vec<String>>,
}
