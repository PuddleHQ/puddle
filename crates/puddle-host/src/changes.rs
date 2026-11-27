// SPDX-License-Identifier: AGPL-3.0-or-later
//! Following the events of the store for changes to a workspace's Git settings, environment and
//! shell init, so that a change applies to the running workspace at once.
//!
//! The events come from a lossless feed of the event hub ([`puddle_api::EventHub::follow`]), not
//! from the stream the UI reads, which drops events for a reader that falls behind. Each
//! workspace has its own queue of changes, run one at a time in the order they came: a guest
//! that is slow to answer delays only its own changes, and two changes never rewrite one guest
//! at once. A change that is already waiting in the queue is not queued twice: it reads the
//! settings when it runs, so one run applies them all.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex, PoisonError};

use puddle_store::WorkspaceGit;
use puddle_types::{Event, PendingEnd, WorkspaceId};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::task::{JoinHandle, JoinSet};

/// What a running workspace does with a change to its Git settings, environment or shell init.
pub(crate) trait Changes: Clone + Send + Sync + 'static {
    /// Recomputes what the workspace decrypts, which is quick and must not wait for anything. The
    /// settings when the workspace runs, else `None`.
    fn decrypt_changed(&self, workspace: &WorkspaceId) -> Option<WorkspaceGit>;

    /// Brings the commit authors in the workspace's guest in line with its settings as they are
    /// when this runs, which can take as long as the guest needs to answer.
    fn rewrite_authors(&self, workspace: WorkspaceId) -> impl Future<Output = ()> + Send;

    /// Reads the rules on method and path again for every running workspace, after one changed.
    /// Quick, like [`Changes::decrypt_changed`]: the next request sees the new rules.
    fn all_request_rules_changed(&self);

    /// Brings a running workspace's stand-ins and the hosts they need decrypted up to date with its
    /// environment, which can take as long as the credential store needs to answer.
    fn environment_changed(&self, workspace: WorkspaceId) -> impl Future<Output = ()> + Send;

    /// The workspaces a change to every workspace reaches (a global variable changed): the
    /// running ones.
    fn running(&self) -> Vec<WorkspaceId>;

    /// Brings the folder a workspace's guest sees up to date with its saved shell init.
    fn shell_init_changed(&self, workspace: WorkspaceId) -> impl Future<Output = ()> + Send;

    /// A pending request of the workspace was closed, `allowed` or not: a tool install that
    /// waited on it may run again. Quick: what it starts runs on its own.
    fn pending_closed(&self, workspace: &WorkspaceId, id: i64, allowed: bool);
}

/// The events [`follow`] acts on: the feed to give it is `EventHub::follow(wanted)`.
pub(crate) fn wanted(event: &Event) -> bool {
    matches!(
        event,
        Event::WorkspaceGitChanged { .. }
            | Event::WorkspaceEnvChanged { .. }
            | Event::WorkspaceShellInitChanged { .. }
            | Event::GlobalEnvChanged {}
            | Event::RequestRulesChanged {}
            | Event::PendingClosed { .. }
    )
}

/// What a workspace's queue runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Change {
    Authors,
    Environment,
    ShellInit,
}

/// The changes waiting per workspace. A workspace is in the map while its queue's task runs; the
/// task takes the changes one by one and leaves the map when it finds none.
type Waiting = Arc<Mutex<HashMap<WorkspaceId, Vec<Change>>>>;

/// Applies each [`Event::WorkspaceGitChanged`], [`Event::RequestRulesChanged`],
/// [`Event::WorkspaceEnvChanged`], [`Event::WorkspaceShellInitChanged`],
/// [`Event::GlobalEnvChanged`] and [`Event::PendingClosed`] of `events` to `service` until the
/// feed ends or the task is aborted, which ends every queue with it. A Git change's decrypt set
/// follows in the order the changes arrive; the guest rewrites and the reads of the credential
/// store go to the workspace's queue.
pub(crate) fn follow<G: Changes>(
    mut events: UnboundedReceiver<Event>,
    service: G,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let waiting = Waiting::default();
        let mut queues = JoinSet::new();
        let queue = |workspace: WorkspaceId, change: Change, queues: &mut JoinSet<()>| {
            enqueue(&waiting, &service, queues, workspace, change);
        };
        loop {
            tokio::select! {
                event = events.recv() => match event {
                    Some(Event::WorkspaceGitChanged { workspace }) => {
                        if service.decrypt_changed(&workspace).is_some() {
                            queue(workspace, Change::Authors, &mut queues);
                        }
                    }
                    Some(Event::WorkspaceEnvChanged { workspace }) => {
                        queue(workspace, Change::Environment, &mut queues);
                    }
                    Some(Event::WorkspaceShellInitChanged { workspace }) => {
                        queue(workspace, Change::ShellInit, &mut queues);
                    }
                    Some(Event::GlobalEnvChanged {}) => {
                        for workspace in service.running() {
                            queue(workspace, Change::Environment, &mut queues);
                        }
                    }
                    Some(Event::RequestRulesChanged {}) => service.all_request_rules_changed(),
                    Some(Event::PendingClosed { workspace, id, state, .. }) => {
                        service.pending_closed(&workspace, id, state == PendingEnd::Allowed);
                    }
                    Some(_) => {}
                    None => break,
                },
                Some(_) = queues.join_next() => {}
            }
        }
    })
}

/// Adds `change` to `workspace`'s queue, unless it already waits there, and starts the queue's
/// task when none runs.
fn enqueue<G: Changes>(
    waiting: &Waiting,
    service: &G,
    queues: &mut JoinSet<()>,
    workspace: WorkspaceId,
    change: Change,
) {
    let mut map = waiting.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(queued) = map.get_mut(&workspace) {
        if !queued.contains(&change) {
            queued.push(change);
        }
        return;
    }
    map.insert(workspace.clone(), vec![change]);
    drop(map);
    let (waiting, service) = (Arc::clone(waiting), service.clone());
    queues.spawn(async move {
        let _leave = LeaveOnPanic {
            waiting: &waiting,
            workspace: &workspace,
        };
        while let Some(change) = next(&waiting, &workspace) {
            match change {
                Change::Authors => service.rewrite_authors(workspace.clone()).await,
                Change::Environment => service.environment_changed(workspace.clone()).await,
                Change::ShellInit => service.shell_init_changed(workspace.clone()).await,
            }
        }
    });
}

/// Takes a queue whose task panicked out of the map, so the next change to its workspace starts a
/// new task instead of waiting behind one that is gone. A task that ends normally has left the
/// map in [`next`] already, and the entry may by then be a newer task's.
struct LeaveOnPanic<'a> {
    waiting: &'a Waiting,
    workspace: &'a WorkspaceId,
}

impl Drop for LeaveOnPanic<'_> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.waiting
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(self.workspace);
        }
    }
}

/// The next change of `workspace`'s queue, or `None` after taking the workspace out of the map
/// (under the same lock, so a change queued later starts a new task).
fn next(waiting: &Waiting, workspace: &WorkspaceId) -> Option<Change> {
    let mut map = waiting.lock().unwrap_or_else(PoisonError::into_inner);
    let queued = map.get_mut(workspace)?;
    if queued.is_empty() {
        map.remove(workspace);
        return None;
    }
    Some(queued.remove(0))
}

#[cfg(test)]
mod tests;
