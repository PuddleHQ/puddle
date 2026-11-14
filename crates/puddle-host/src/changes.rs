// SPDX-License-Identifier: AGPL-3.0-or-later
//! Following the events of the store for changes to a workspace's Git settings and environment,
//! so that a change applies to the running workspace at once.

use std::future::Future;

use puddle_store::WorkspaceGit;
use puddle_types::{Event, WorkspaceId};
use tokio::sync::broadcast::{Receiver, error::RecvError};
use tokio::task::{JoinHandle, JoinSet};

/// What a running workspace does with a change to its Git settings or its environment.
pub(crate) trait Changes: Clone + Send + Sync + 'static {
    /// Recomputes what the workspace decrypts, which is quick and must not wait for anything. The
    /// settings when the workspace runs, else `None`.
    fn decrypt_changed(&self, workspace: &WorkspaceId) -> Option<WorkspaceGit>;

    /// Brings the commit authors in the workspace's guest in line with its settings as they are
    /// when this runs, which can take as long as the guest needs to answer. Rewrites of one
    /// workspace start in any order, so each reads the settings itself rather than carry a copy:
    /// an older copy applied last would undo the newer change.
    fn rewrite_authors(&self, workspace: WorkspaceId) -> impl Future<Output = ()> + Send;

    /// Looks at every running workspace's Git settings again, after events were missed.
    fn all_git_changed(&self) -> impl Future<Output = ()> + Send;

    /// Reads the rules on method and path again for every running workspace, after one changed.
    /// Quick, like [`Changes::decrypt_changed`]: the next request sees the new rules.
    fn all_request_rules_changed(&self);

    /// Brings a running workspace's stand-ins and the hosts they need decrypted up to date with its
    /// environment, which can take as long as the credential store needs to answer.
    fn environment_changed(&self, workspace: WorkspaceId) -> impl Future<Output = ()> + Send;

    /// The same for every running workspace: a global variable changed, or events were missed.
    fn all_environments_changed(&self) -> impl Future<Output = ()> + Send;

    /// Brings the folder a workspace's guest sees up to date with its saved shell init.
    fn shell_init_changed(&self, workspace: WorkspaceId) -> impl Future<Output = ()> + Send;

    /// The same for every workspace, after events were missed.
    fn all_shell_inits_changed(&self) -> impl Future<Output = ()> + Send;
}

/// Applies each [`Event::WorkspaceGitChanged`], [`Event::RequestRulesChanged`], [`Event::WorkspaceEnvChanged`],
/// [`Event::WorkspaceShellInitChanged`] and [`Event::GlobalEnvChanged`] of `events` to `service`
/// until the task is aborted. A Git change's
/// decrypt set follows in the order the changes arrive; the guest rewrites and the reads of the
/// credential store run on their own, so a guest or a store that is slow to answer delays only
/// itself.
pub(crate) fn follow<G: Changes>(mut events: Receiver<Event>, service: G) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut rewrites = JoinSet::new();
        loop {
            tokio::select! {
                event = events.recv() => match event {
                    Ok(Event::WorkspaceGitChanged { workspace }) => {
                        if service.decrypt_changed(&workspace).is_some() {
                            let service = service.clone();
                            rewrites.spawn(async move {
                                service.rewrite_authors(workspace).await;
                            });
                        }
                    }
                    Ok(Event::WorkspaceEnvChanged { workspace }) => {
                        let service = service.clone();
                        rewrites.spawn(async move {
                            service.environment_changed(workspace).await;
                        });
                    }
                    Ok(Event::WorkspaceShellInitChanged { workspace }) => {
                        let service = service.clone();
                        rewrites.spawn(async move {
                            service.shell_init_changed(workspace).await;
                        });
                    }
                    Ok(Event::GlobalEnvChanged {}) => {
                        let service = service.clone();
                        rewrites.spawn(async move {
                            service.all_environments_changed().await;
                        });
                    }
                    Ok(Event::RequestRulesChanged {}) => service.all_request_rules_changed(),
                    Ok(_) => {}
                    Err(RecvError::Lagged(_)) => {
                        service.all_git_changed().await;
                        let service = service.clone();
                        rewrites.spawn(async move {
                            service.all_environments_changed().await;
                            service.all_shell_inits_changed().await;
                        });
                    }
                    Err(RecvError::Closed) => break,
                },
                Some(_) = rewrites.join_next() => {}
            }
        }
    })
}

#[cfg(test)]
mod tests;
