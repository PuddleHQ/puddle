// SPDX-License-Identifier: AGPL-3.0-or-later
//! The editor connections open per workspace. A workbench window keeps at least one WebSocket to
//! its server for as long as it is connected (and a window that crashed or lost its network ends
//! its sockets with the operating system), so "no connection" means no window is connected, which
//! is what an update of the server waits for.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use puddle_types::WorkspaceId;
use tokio::sync::watch;

/// The connection counts of every workspace. Cheap to clone; clones share the counts.
#[derive(Debug, Clone, Default)]
pub struct Windows {
    counts: Arc<Mutex<HashMap<WorkspaceId, watch::Sender<usize>>>>,
}

impl Windows {
    fn sender(&self, workspace: &WorkspaceId) -> watch::Sender<usize> {
        let mut counts = self.counts.lock().unwrap_or_else(PoisonError::into_inner);
        counts
            .entry(workspace.clone())
            .or_insert_with(|| watch::channel(0).0)
            .clone()
    }

    /// Counts one more connection of `workspace` until the returned guard is dropped.
    #[must_use]
    pub fn open(&self, workspace: &WorkspaceId) -> Connected {
        let sender = self.sender(workspace);
        sender.send_modify(|n| *n += 1);
        Connected { sender }
    }

    /// How many connections `workspace` has now.
    #[must_use]
    pub fn connected(&self, workspace: &WorkspaceId) -> usize {
        *self.sender(workspace).borrow()
    }

    /// The count as it changes, for waiting until it is zero.
    #[must_use]
    pub fn watch(&self, workspace: &WorkspaceId) -> watch::Receiver<usize> {
        self.sender(workspace).subscribe()
    }
}

/// One open connection; the count drops when this does.
#[derive(Debug)]
pub struct Connected {
    sender: watch::Sender<usize>,
}

impl Drop for Connected {
    fn drop(&mut self) {
        self.sender.send_modify(|n| *n = n.saturating_sub(1));
    }
}

#[cfg(test)]
mod tests;
