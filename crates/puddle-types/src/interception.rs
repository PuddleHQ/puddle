// SPDX-License-Identifier: AGPL-3.0-or-later
//! How Puddle decrypts port 443: the three modes of the global setting and of a workspace's
//! override.

use serde::{Deserialize, Serialize};

/// Which hosts on port 443 Puddle decrypts, beyond the ones with a credential, secret, login
/// capture profile or rule on method and path (those are always decrypted).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub enum InterceptionMode {
    /// Every host. A tool with its own trust store fails on first contact until Puddle learns
    /// the host.
    All,
    /// Every host except the curated list of hosts known to break, which is passed through.
    AllExceptCurated,
    /// Only the hosts Puddle has a reason to read. A rule on any other host can only allow or
    /// deny the whole host.
    KnownOnly,
}

impl InterceptionMode {
    /// The name stored in the database and sent over the API.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::AllExceptCurated => "all_except_curated",
            Self::KnownOnly => "known_only",
        }
    }

    /// The mode a stored name stands for.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "all" => Some(Self::All),
            "all_except_curated" => Some(Self::AllExceptCurated),
            "known_only" => Some(Self::KnownOnly),
            _ => None,
        }
    }

    /// Whether this mode decrypts hosts it has no reason to read.
    #[must_use]
    pub fn decrypts_unknown_hosts(self) -> bool {
        !matches!(self, Self::KnownOnly)
    }

    /// The mode of an install that has not chosen: today's behaviour.
    pub const EXISTING_INSTALL: Self = Self::KnownOnly;

    /// The mode preselected in the first-run step, and taken when the step is skipped.
    pub const FIRST_RUN_DEFAULT: Self = Self::AllExceptCurated;
}

/// The hosts passed through in [`InterceptionMode::AllExceptCurated`]: names (exact, or `*.` and
/// a domain) whose clients are known to refuse Puddle's certificate for them. "System managed":
/// the user cannot edit this list, but can add names of their own to the never-decrypt list.
pub const CURATED_PASS_THROUGH: &[&str] = &[];

#[cfg(test)]
mod tests;
