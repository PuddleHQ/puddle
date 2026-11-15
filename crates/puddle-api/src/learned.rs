// SPDX-License-Identifier: AGPL-3.0-or-later
//! The hosts a workspace's tools refused Puddle's certificate for, which Puddle then stopped
//! decrypting: the [`LearnedService`] trait the routes call, [`NoLearned`] for a build without
//! it, and [`FakeLearned`] for tests and the UI fixture.
//!
//! The pass-through lives in memory in the running workspace and the history of refusals in the
//! database, so both are read and changed through the host.

use std::collections::BTreeMap;
use std::sync::{Mutex, PoisonError};

use puddle_types::WorkspaceId;

use crate::wire::LearnedHost;

/// What the learned list asks of the host. Workspaces are named by their stored name; a
/// workspace that does not run passes nothing through but keeps its history.
pub trait LearnedService: Send + Sync {
    /// The workspace's learned hosts: the ones passed through now first (soonest to expire
    /// first), then the others, the most recently refused first.
    fn list(&self, workspace: &WorkspaceId) -> Vec<LearnedHost>;

    /// Makes `host` (already normalised) decrypted again from the next connection; its history
    /// stays. `false` when it was not passed through.
    fn forget(&self, workspace: &WorkspaceId, host: &str) -> bool;

    /// Decrypts every host of the workspace again; how many were passed through.
    fn clear(&self, workspace: &WorkspaceId) -> usize;

    /// Removes `host` (already normalised) from the list altogether, history included, once the
    /// caller has made it permanent elsewhere. `false` when it was not listed.
    fn discard(&self, workspace: &WorkspaceId, host: &str) -> bool;
}

/// The service when none is wired in: nothing is ever learned.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoLearned;

impl LearnedService for NoLearned {
    fn list(&self, _: &WorkspaceId) -> Vec<LearnedHost> {
        Vec::new()
    }

    fn forget(&self, _: &WorkspaceId, _: &str) -> bool {
        false
    }

    fn clear(&self, _: &WorkspaceId) -> usize {
        0
    }

    fn discard(&self, _: &WorkspaceId, _: &str) -> bool {
        false
    }
}

/// An in-memory service for tests and the UI fixture: hosts are added by hand with the history
/// of their refusals.
#[derive(Debug, Default)]
pub struct FakeLearned {
    state: Mutex<BTreeMap<WorkspaceId, Vec<LearnedHost>>>,
}

impl FakeLearned {
    /// A fake with nothing learned.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds `host` to `workspace`'s list, passed through with `expires_in_secs` left, refused
    /// once (at time 1).
    pub fn learn(&self, workspace: &WorkspaceId, host: &str, expires_in_secs: u64) {
        self.learn_with_history(workspace, host, Some(expires_in_secs), 1, 1);
    }

    /// Adds `host` with a history: `expires_in_secs` is `Some` while it is passed through, and
    /// it was refused `count` times, the last at `last_rejected_at` (the first one second
    /// earlier per extra refusal, at the earliest at 0).
    pub fn learn_with_history(
        &self,
        workspace: &WorkspaceId,
        host: &str,
        expires_in_secs: Option<u64>,
        count: u32,
        last_rejected_at: u64,
    ) {
        let first = last_rejected_at.saturating_sub(u64::from(count.saturating_sub(1)) * 1000);
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let rows = state.entry(workspace.clone()).or_default();
        rows.retain(|row| row.host != host);
        rows.push(LearnedHost::new(
            host.to_owned(),
            expires_in_secs,
            first,
            last_rejected_at,
            count,
        ));
    }
}

impl LearnedService for FakeLearned {
    fn list(&self, workspace: &WorkspaceId) -> Vec<LearnedHost> {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let mut rows = state.get(workspace).cloned().unwrap_or_default();
        rows.sort_by(|a, b| {
            let key = |h: &LearnedHost| {
                (
                    h.expires_in_secs.is_none(),
                    h.expires_in_secs,
                    std::cmp::Reverse(h.last_rejected_at),
                    h.host.clone(),
                )
            };
            key(a).cmp(&key(b))
        });
        rows
    }

    fn forget(&self, workspace: &WorkspaceId, host: &str) -> bool {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(row) = state
            .get_mut(workspace)
            .and_then(|rows| rows.iter_mut().find(|row| row.host == host))
            .filter(|row| row.passed_through)
        else {
            return false;
        };
        *row = LearnedHost::new(
            row.host.clone(),
            None,
            row.first_rejected_at,
            row.last_rejected_at,
            row.count,
        );
        true
    }

    fn clear(&self, workspace: &WorkspaceId) -> usize {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let mut cleared = 0;
        for row in state.entry(workspace.clone()).or_default() {
            if row.passed_through {
                cleared += 1;
                *row = LearnedHost::new(
                    row.host.clone(),
                    None,
                    row.first_rejected_at,
                    row.last_rejected_at,
                    row.count,
                );
            }
        }
        cleared
    }

    fn discard(&self, workspace: &WorkspaceId, host: &str) -> bool {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(rows) = state.get_mut(workspace) else {
            return false;
        };
        let before = rows.len();
        rows.retain(|row| row.host != host);
        rows.len() != before
    }
}

#[cfg(test)]
mod tests;
