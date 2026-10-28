// SPDX-License-Identifier: GPL-3.0-or-later
//! The hosts a workspace's tools refused Puddle's certificate for, which Puddle then stopped
//! decrypting: the [`LearnedService`] trait the routes call, [`NoLearned`] for a build without
//! it, and [`FakeLearned`] for tests and the UI fixture.
//!
//! The list lives in memory in the running workspace, so it is read and changed through the host,
//! not the database.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Mutex, PoisonError};

use puddle_types::WorkspaceName;

use crate::wire::LearnedHost;

/// What the learned list asks of the host. Workspaces are named by their stored name; a
/// workspace that does not run has an empty list.
pub trait LearnedService: Send + Sync {
    /// The workspace's learned hosts, soonest to expire first.
    fn list(&self, workspace: &WorkspaceName) -> Vec<LearnedHost>;

    /// Makes `host` (already normalised) decrypted again from the next connection. `false` when
    /// it was not on the list.
    fn forget(&self, workspace: &WorkspaceName, host: &str) -> bool;

    /// Empties the workspace's list; how many hosts it held.
    fn clear(&self, workspace: &WorkspaceName) -> usize;
}

/// The service when none is wired in: nothing is ever learned.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoLearned;

impl LearnedService for NoLearned {
    fn list(&self, _: &WorkspaceName) -> Vec<LearnedHost> {
        Vec::new()
    }

    fn forget(&self, _: &WorkspaceName, _: &str) -> bool {
        false
    }

    fn clear(&self, _: &WorkspaceName) -> usize {
        0
    }
}

/// An in-memory service for tests and the UI fixture: hosts are added by hand with the seconds
/// they have left.
#[derive(Debug, Default)]
pub struct FakeLearned {
    state: Mutex<BTreeMap<WorkspaceName, BTreeSet<(u64, String)>>>,
}

impl FakeLearned {
    /// A fake with nothing learned.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds `host` to `workspace`'s list with `expires_in_secs` left.
    pub fn learn(&self, workspace: &WorkspaceName, host: &str, expires_in_secs: u64) {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(workspace.clone())
            .or_default()
            .insert((expires_in_secs, host.to_owned()));
    }
}

impl LearnedService for FakeLearned {
    fn list(&self, workspace: &WorkspaceName) -> Vec<LearnedHost> {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state
            .get(workspace)
            .into_iter()
            .flatten()
            .map(|(secs, host)| LearnedHost {
                host: host.clone(),
                expires_in_secs: *secs,
            })
            .collect()
    }

    fn forget(&self, workspace: &WorkspaceName, host: &str) -> bool {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(rows) = state.get_mut(workspace) else {
            return false;
        };
        let before = rows.len();
        rows.retain(|(_, name)| name != host);
        rows.len() != before
    }

    fn clear(&self, workspace: &WorkspaceName) -> usize {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.remove(workspace).map_or(0, |rows| rows.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws(name: &str) -> WorkspaceName {
        WorkspaceName::new(name).unwrap()
    }

    #[test]
    fn a_build_without_a_host_has_nothing_learned() {
        let none = NoLearned;
        assert_eq!(none.list(&ws("a")).len(), 0);
        assert!(!none.forget(&ws("a"), "x.example"));
        assert_eq!(none.clear(&ws("a")), 0);
    }

    #[test]
    fn the_fake_lists_soonest_first_and_forgets_by_workspace() {
        let fake = FakeLearned::new();
        fake.learn(&ws("a"), "late.example", 90);
        fake.learn(&ws("a"), "soon.example", 10);
        fake.learn(&ws("b"), "soon.example", 10);
        let hosts: Vec<_> = fake.list(&ws("a")).into_iter().map(|h| h.host).collect();
        assert_eq!(hosts, ["soon.example", "late.example"]);
        assert!(!fake.forget(&ws("c"), "soon.example"));
        assert!(fake.forget(&ws("a"), "soon.example"));
        assert!(!fake.forget(&ws("a"), "soon.example"));
        assert_eq!(
            fake.list(&ws("b")).len(),
            1,
            "another workspace is untouched"
        );
        assert_eq!(fake.clear(&ws("a")), 1);
        assert_eq!(fake.clear(&ws("a")), 0);
    }
}
