// SPDX-License-Identifier: AGPL-3.0-or-later
//! What a client says about itself so the server can pick who is told about a new request.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// What kind of client reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ClientKind {
    /// A page in a browser.
    Browser,
    /// The page inside the desktop app's window; the app raises its own operating-system
    /// notification for it.
    Desktop,
}

/// One client's report: whether it is in front, when it last saw input, and whether the person
/// wants notifications there. Sent every 30 seconds and whenever one of them changes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PresenceReport {
    /// A random id the client made for itself (1 to 64 characters: letters, digits, `-`, `_`).
    /// A browser tab keeps one for as long as it is open.
    pub client: String,
    /// What kind of client this is.
    pub kind: ClientKind,
    /// Whether the page is in front (visible) now.
    pub visible: bool,
    /// How long ago the last keystroke, click or touch was, in milliseconds. Sent as an age so
    /// the client's clock does not matter.
    pub idle_ms: u64,
    /// Whether the person wants notifications on this client (its own switch).
    pub notify: bool,
}
