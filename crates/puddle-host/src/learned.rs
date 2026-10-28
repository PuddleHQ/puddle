// SPDX-License-Identifier: GPL-3.0-or-later
//! The host's side of the learned pass-through list: the service the API reads and edits it
//! through. The list lives in memory in each running workspace's [`Interception`]; it is reached
//! through the registry of terminations the proxy asks.

use std::sync::Arc;

use puddle_api::LearnedService;
use puddle_api::wire::LearnedHost;
use puddle_proxy::{PassThrough, TerminationSource, Terminations};
use puddle_types::{Host, WorkspaceName};

/// The learned list of every running workspace.
pub(crate) struct HostLearned {
    terminations: Arc<Terminations>,
}

impl HostLearned {
    pub(crate) fn new(terminations: Arc<Terminations>) -> Self {
        Self { terminations }
    }

    fn list_of(&self, workspace: &WorkspaceName) -> Option<Arc<PassThrough>> {
        self.terminations
            .termination(workspace)
            .map(|t| Arc::clone(t.interception().learned()))
    }
}

impl LearnedService for HostLearned {
    fn list(&self, workspace: &WorkspaceName) -> Vec<LearnedHost> {
        self.list_of(workspace)
            .map(|list| {
                list.list()
                    .into_iter()
                    .map(|(host, left)| LearnedHost {
                        host,
                        // Round up: a host with 400 ms left is not "expired in 0 s".
                        expires_in_secs: left.as_secs() + u64::from(left.subsec_nanos() > 0),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn forget(&self, workspace: &WorkspaceName, host: &str) -> bool {
        let Ok(host) = Host::parse_normalised(host) else {
            return false;
        };
        self.list_of(workspace)
            .is_some_and(|list| list.forget(&host))
    }

    fn clear(&self, workspace: &WorkspaceName) -> usize {
        self.list_of(workspace).map_or(0, |list| list.clear())
    }
}
