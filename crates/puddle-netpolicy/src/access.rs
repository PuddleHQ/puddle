// SPDX-License-Identifier: AGPL-3.0-or-later
//! The local-destination settings one workspace runs with: a toggle per category and
//! whether wildcard allows reach local addresses.

use puddle_settings::Effective;
use puddle_types::{LocalCategory, WorkspaceId};

/// One workspace's local-destination settings, resolved (workspace override over global over
/// default). [`LocalAccess::NONE`] is Puddle's default: every toggle off, wildcards don't reach
/// local addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct LocalAccess {
    on: u8,
    wildcards_reach_local: bool,
    unreadable: bool,
}

fn bit(category: LocalCategory) -> u8 {
    LocalCategory::ALL
        .iter()
        .position(|c| *c == category)
        .map_or(0, |i| 1 << i)
}

impl LocalAccess {
    /// Every toggle off, wildcards don't reach local addresses.
    pub const NONE: Self = Self {
        on: 0,
        wildcards_reach_local: false,
        unreadable: false,
    };

    /// [`Self::NONE`] because the settings could not be read, not because they say so: the guard
    /// blocks the same destinations but its refusal names the unreadable settings.
    pub const UNREADABLE: Self = Self {
        unreadable: true,
        ..Self::NONE
    };

    /// The values from resolved settings.
    #[must_use]
    pub fn from_effective(effective: &Effective) -> Self {
        LocalCategory::ALL.into_iter().fold(
            Self::NONE.with_wildcards_reach_local(effective.wildcards_reach_local.value),
            |access, c| access.with_toggle(c, effective.local_toggles.get(c).value),
        )
    }

    /// The same with `category`'s toggle set to `on`.
    #[must_use]
    pub fn with_toggle(mut self, category: LocalCategory, on: bool) -> Self {
        if on {
            self.on |= bit(category);
        } else {
            self.on &= !bit(category);
        }
        self
    }

    /// The same with "wildcards reach local addresses" set to `on`.
    #[must_use]
    pub fn with_wildcards_reach_local(mut self, on: bool) -> Self {
        self.wildcards_reach_local = on;
        self
    }

    /// Whether `category`'s toggle is on.
    #[must_use]
    pub fn is_on(self, category: LocalCategory) -> bool {
        bit(category) != 0 && self.on & bit(category) != 0
    }

    /// Whether this is [`Self::UNREADABLE`]: every toggle is off because the settings could not
    /// be read.
    #[must_use]
    pub fn is_unreadable(self) -> bool {
        self.unreadable
    }

    /// Whether a wildcard (suffix) allow may reach a local address whose toggle is on.
    #[must_use]
    pub fn wildcards_reach_local(self) -> bool {
        self.wildcards_reach_local
    }
}

/// Where the guard gets each workspace's [`LocalAccess`], read per request so a settings change
/// applies to the next connection. The host program implements it over the settings store; a
/// fixed [`LocalAccess`] applies to every workspace, and so does a closure.
pub trait LocalAccessSource: Send + Sync {
    /// The settings `workspace` runs with now.
    fn local_access(&self, workspace: &WorkspaceId) -> LocalAccess;
}

impl LocalAccessSource for LocalAccess {
    fn local_access(&self, _workspace: &WorkspaceId) -> LocalAccess {
        *self
    }
}

impl<F> LocalAccessSource for F
where
    F: Fn(&WorkspaceId) -> LocalAccess + Send + Sync,
{
    fn local_access(&self, workspace: &WorkspaceId) -> LocalAccess {
        self(workspace)
    }
}

#[cfg(test)]
mod tests;
