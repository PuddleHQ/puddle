// SPDX-License-Identifier: AGPL-3.0-or-later
//! The host's side of the learned list: the service the API reads and edits it through. The
//! pass-through lives in memory in each running workspace's [`Interception`]; it is reached
//! through the registry of terminations the proxy asks. The history of refusals (first and last
//! time, how many) is in the store, so it outlives the pass-through and a restart.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use puddle_api::LearnedService;
use puddle_api::wire::LearnedHost;
use puddle_proxy::{PassThrough, TerminationSource, Terminations};
use puddle_store::Store;
use puddle_types::{Host, WorkspaceId};

/// The learned list of every workspace.
pub(crate) struct HostLearned {
    terminations: Arc<Terminations>,
    store: Arc<Store>,
}

impl HostLearned {
    pub(crate) fn new(terminations: Arc<Terminations>, store: Arc<Store>) -> Self {
        Self {
            terminations,
            store,
        }
    }

    fn list_of(&self, workspace: &WorkspaceId) -> Option<Arc<PassThrough>> {
        self.terminations
            .termination(workspace)
            .map(|t| Arc::clone(t.interception().learned()))
    }
}

/// Seconds left, rounded up: a host with 400 ms left is not "expired in 0 s".
fn whole_secs(left: std::time::Duration) -> u64 {
    left.as_secs() + u64::from(left.subsec_nanos() > 0)
}

impl LearnedService for HostLearned {
    fn list(&self, workspace: &WorkspaceId) -> Vec<LearnedHost> {
        let mut passing: HashMap<String, u64> = self
            .list_of(workspace)
            .map(|list| {
                list.list()
                    .into_iter()
                    .map(|(host, left)| (host, whole_secs(left)))
                    .collect()
            })
            .unwrap_or_default();
        let history = self.store.learned_history(workspace).unwrap_or_else(|e| {
            tracing::warn!(%workspace, error = %e, "cannot read the history of refused hosts");
            Vec::new()
        });
        let mut hosts: Vec<LearnedHost> = history
            .into_iter()
            .map(|row| {
                let left = passing.remove(&row.host);
                LearnedHost::new(row.host, left, row.first_at, row.last_at, row.count)
            })
            .collect();
        // A host passed through whose history could not be written is still shown.
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
        hosts.extend(
            passing
                .into_iter()
                .map(|(host, left)| LearnedHost::new(host, Some(left), now, now, 1)),
        );
        hosts.sort_by(|a, b| {
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
        hosts
    }

    fn forget(&self, workspace: &WorkspaceId, host: &str) -> bool {
        let Ok(host) = Host::parse_normalised(host) else {
            return false;
        };
        self.list_of(workspace)
            .is_some_and(|list| list.forget(&host))
    }

    fn clear(&self, workspace: &WorkspaceId) -> usize {
        self.list_of(workspace).map_or(0, |list| list.clear())
    }

    fn discard(&self, workspace: &WorkspaceId, host: &str) -> bool {
        let Ok(name) = Host::parse_normalised(host) else {
            return false;
        };
        let was_passed_through = self
            .list_of(workspace)
            .is_some_and(|list| list.forget(&name));
        let recorded = self
            .store
            .drop_learned_host(workspace, host)
            .unwrap_or_else(|e| {
                tracing::warn!(%workspace, host, error = %e, "cannot drop the history of a refused host");
                false
            });
        was_passed_through || recorded
    }
}
