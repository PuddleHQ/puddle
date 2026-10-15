// SPDX-License-Identifier: GPL-3.0-or-later
//! A workspace's ports on the wire: what listens in it, what is forwarded to this computer, what
//! is pinned. Every field is always present (ADR 0002).

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// One port of a workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PortEntry {
    /// The port in the workspace.
    pub guest_port: u16,
    /// A process in the workspace listens on it now. Reported by the workspace itself, so a
    /// workspace that is not running lists none, and one that hides a listener hides it here.
    pub listening: bool,
    /// A forward to it is open now: connecting to `host_port` on this computer reaches it.
    pub forwarded: bool,
    /// The forward comes back, on the same `host_port`, every time the workspace starts.
    pub pinned: bool,
    /// The port on this computer: the open forward's, or the one a pinned forward wants. `null`
    /// for a port that is neither forwarded nor pinned.
    #[schema(required = true)]
    pub host_port: Option<u16>,
    /// Why it does not work: the pinned forward could not be restored (the port of this computer
    /// is in use, and by whom), or the last connection through it failed. May quote the guest's
    /// or the system's words: escape it when rendering. `null` when there is no problem.
    #[schema(required = true)]
    pub problem: Option<String>,
}

/// What a workspace's Ports tab lists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkspacePorts {
    /// The workspace runs: ports can be forwarded. A stopped workspace lists its pinned forwards
    /// only.
    pub running: bool,
    /// The ports, by number: the ones that listen, the ones forwarded, the ones pinned.
    pub ports: Vec<PortEntry>,
}

/// Forwards a port of the workspace to this computer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PortForwardRequest {
    /// The port on this computer, 1 to 65535. Left out or `null`: the workspace port's own number.
    #[serde(default)]
    pub host_port: Option<u16>,
    /// Keep the forward across restarts of the workspace (default false). Asking again with
    /// `true` pins a forward that is open; with `false` it unpins one and leaves it open until
    /// the workspace stops.
    #[serde(default)]
    pub pinned: bool,
}
