// SPDX-License-Identifier: GPL-3.0-or-later
//! The editor connections open per workspace. A workbench window keeps at least one WebSocket to
//! its server for as long as it is connected (and a window that crashed or lost its network ends
//! its sockets with the operating system), so "no connection" means no window is connected, which
//! is what an update of the server waits for.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use puddle_types::WorkspaceName;
use tokio::sync::watch;

/// The connection counts of every workspace. Cheap to clone; clones share the counts.
#[derive(Debug, Clone, Default)]
pub struct Windows {
    counts: Arc<Mutex<HashMap<WorkspaceName, watch::Sender<usize>>>>,
}

impl Windows {
    fn sender(&self, workspace: &WorkspaceName) -> watch::Sender<usize> {
        let mut counts = self.counts.lock().unwrap_or_else(PoisonError::into_inner);
        counts
            .entry(workspace.clone())
            .or_insert_with(|| watch::channel(0).0)
            .clone()
    }

    /// Counts one more connection of `workspace` until the returned guard is dropped.
    #[must_use]
    pub fn open(&self, workspace: &WorkspaceName) -> Connected {
        let sender = self.sender(workspace);
        sender.send_modify(|n| *n += 1);
        Connected { sender }
    }

    /// How many connections `workspace` has now.
    #[must_use]
    pub fn connected(&self, workspace: &WorkspaceName) -> usize {
        *self.sender(workspace).borrow()
    }

    /// The count as it changes, for waiting until it is zero.
    #[must_use]
    pub fn watch(&self, workspace: &WorkspaceName) -> watch::Receiver<usize> {
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
mod tests {
    use super::*;

    #[test]
    fn the_count_follows_the_guards_per_workspace() {
        let windows = Windows::default();
        let (a, b) = (
            WorkspaceName::new("a").unwrap(),
            WorkspaceName::new("b").unwrap(),
        );
        let one = windows.open(&a);
        let two = windows.open(&a);
        let other = windows.open(&b);
        assert_eq!((windows.connected(&a), windows.connected(&b)), (2, 1));
        drop(one);
        assert_eq!(windows.connected(&a), 1);
        drop((two, other));
        assert_eq!((windows.connected(&a), windows.connected(&b)), (0, 0));
    }

    #[tokio::test]
    async fn a_watcher_sees_the_count_reach_zero() {
        let windows = Windows::default();
        let a = WorkspaceName::new("a").unwrap();
        let guard = windows.open(&a);
        let mut seen = windows.watch(&a);
        let wait = tokio::spawn(async move { seen.wait_for(|n| *n == 0).await.map(|n| *n) });
        drop(guard);
        assert_eq!(wait.await.unwrap().unwrap(), 0);
    }
}
