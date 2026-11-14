// SPDX-License-Identifier: AGPL-3.0-or-later
//! The guest user a workspace's sessions, browser editor and clones run as.
//!
//! `agent` is a plain user with passwordless `sudo` that the boot hook adds when the image has
//! none; `root` is what every workspace ran as before `agent` existed, and what a workspace made
//! then keeps.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Who sessions run as in the guest. The default is the one for a new workspace; a boot plan that
/// names no user stays `root` (see `puddle-boot`), so nothing changes for a caller that never asks.
///
/// ```
/// use puddle_types::SessionUser;
/// assert_eq!(SessionUser::default(), SessionUser::Agent);
/// assert_eq!(SessionUser::Agent.name(), "agent");
/// assert_eq!(SessionUser::Agent.home(), "/home/agent");
/// assert_eq!(SessionUser::Root.home(), "/root");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum SessionUser {
    /// `agent`, with passwordless `sudo`: the default for a new workspace.
    #[default]
    Agent,
    /// `root`: a workspace made before `agent` existed, or one the user switched back.
    Root,
}

impl SessionUser {
    /// The user name in the guest.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Root => "root",
        }
    }

    /// The user's home in the guest, where Puddle writes per-user tool settings.
    #[must_use]
    pub const fn home(self) -> &'static str {
        match self {
            Self::Agent => "/home/agent",
            Self::Root => "/root",
        }
    }

    /// Whether this is `root`.
    #[must_use]
    pub const fn is_root(self) -> bool {
        matches!(self, Self::Root)
    }
}

impl fmt::Display for SessionUser {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

#[cfg(test)]
mod tests;
