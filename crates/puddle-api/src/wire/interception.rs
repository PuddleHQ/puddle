// SPDX-License-Identifier: GPL-3.0-or-later
//! Decrypting every host on the wire: a workspace's switch and the names that are never decrypted.

use puddle_types::WorkspaceName;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// How a workspace decrypts hosts beyond the ones it has a credential for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct InterceptionView {
    /// The workspace.
    pub workspace: WorkspaceName,
    /// Whether Puddle decrypts every host on port 443, not only the hosts with a credential,
    /// secret or login capture. Off unless turned on.
    pub decrypt_all: bool,
    /// The names this workspace never decrypts while the switch is on (`example.com`,
    /// `*.example.com`), sorted.
    pub never_decrypt: Vec<String>,
    /// The names no workspace decrypts, sorted. Changed through the global route.
    pub global_never_decrypt: Vec<String>,
}

/// A change to a workspace's interception. A field that is absent stays as it is.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
pub struct InterceptionRequest {
    /// Turns decrypting every host on or off. It applies to the running workspace at once.
    #[serde(default)]
    pub decrypt_all: Option<bool>,
    /// Replaces the workspace's never-decrypt names. Each is a host name or `*.` followed by a
    /// domain with at least two labels; no IP addresses. At most 500.
    #[serde(default)]
    pub never_decrypt: Option<Vec<String>>,
}

/// The names no workspace decrypts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct GlobalNeverDecrypt {
    /// The names, as for a workspace; sorted. On saving, replaces the whole list. At most 500.
    pub never_decrypt: Vec<String>,
}
