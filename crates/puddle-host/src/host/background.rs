// SPDX-License-Identifier: AGPL-3.0-or-later
//! The tasks that run beside the workspaces and are stopped at shutdown.

use std::sync::Arc;

use puddle_api::EventHub;
use puddle_compute::Runtime;
use puddle_front::Front;
use puddle_proxy::PullRoute;
use puddle_services::{
    network_health::forward_network_changes, network_health::forward_proxy_sign_in_failures,
};
use puddle_store::Sweeper;
use puddle_upstream::{Chain, Discovery, Watching};

use crate::HostWorkspaces;

use super::egress::Injecting;
use super::phases::{Serving, Shared};

pub(super) struct Background {
    pub(super) sweeper: Sweeper,
    // Holds the store; awaited at shutdown so the database is closed when the host is.
    pub(super) vault_sweep: tokio::task::JoinHandle<()>,
    // Also holds the store (the injection removes rows); awaited at shutdown for the same reason.
    pub(super) retired_sweep: tokio::task::JoinHandle<()>,
    pub(super) pull: PullRoute,
    pub(super) front: Front,
    // Dropped (stopped) with the rest; `None` where the OS cannot watch.
    pub(super) _watching: Option<Watching>,
    // Tells the API's event stream about each new network epoch; ended with the rest.
    pub(super) network_events: AbortOnDrop,
    pub(super) sign_in_failures: AbortOnDrop,
    // Applies a change of a running workspace's identities to what it decrypts and its authors.
    pub(super) git_changes: AbortOnDrop,
    // Turns a read that needs a sign-in into the user's notice.
    pub(super) sign_in_notices: AbortOnDrop,
    // Asks Microsoft's editor server to switch to a staged build when no window is on it.
    pub(super) editor_updates: AbortOnDrop,
    // Says when the free space on the drive that holds the data folder crosses a line.
    pub(super) space_watch: AbortOnDrop,
    // Reads the disks' use now and then and warns when one reaches the threshold.
    pub(super) disk_watch: AbortOnDrop,
}

impl Background {
    pub(super) fn new(
        serving: Serving,
        watchers: Watchers,
        vault_sweep: tokio::task::JoinHandle<()>,
    ) -> Self {
        Self {
            sweeper: serving.sweeper,
            vault_sweep,
            retired_sweep: watchers.retired_sweep,
            pull: serving.pull,
            front: serving.front,
            _watching: serving.watching,
            network_events: serving.network_events,
            sign_in_failures: serving.sign_in_failures,
            git_changes: watchers.git_changes,
            sign_in_notices: watchers.sign_in_notices,
            editor_updates: watchers.editor_updates,
            space_watch: watchers.space_watch,
            disk_watch: watchers.disk_watch,
        }
    }
}

/// Waits up to `within` for the clean-up of deleted workspaces, then stops it where it waits: a
/// credential store that does not answer must not hold up a quit. What it left is cleaned at the
/// next start, since an id is noted as cleaned only once all of it is gone.
pub(super) async fn finish_retired_sweep(
    mut sweep: tokio::task::JoinHandle<()>,
    within: std::time::Duration,
) {
    if tokio::time::timeout(within, &mut sweep).await.is_err() {
        tracing::warn!(
            waited = ?within,
            "the clean-up of deleted workspaces did not finish while quitting; the next start goes on with it"
        );
        sweep.abort();
        let _ = sweep.await;
    }
}

/// Aborts its task when dropped, so a forwarder never outlives the host.
pub(super) struct AbortOnDrop(pub(super) tokio::task::JoinHandle<()>);

impl AbortOnDrop {
    /// Announces each new network epoch of `discovery` on `events`.
    pub(super) fn forwarding(discovery: &Discovery, events: &Arc<EventHub>) -> Self {
        Self(forward_network_changes(discovery, events.clone()))
    }
}

impl AbortOnDrop {
    /// Announces on `events` each time signing in to a company proxy begins to fail.
    pub(super) fn forwarding_sign_in_failures(chain: &Chain, events: &Arc<EventHub>) -> Self {
        Self(forward_proxy_sign_in_failures(chain, events.clone()))
    }
}

impl AbortOnDrop {
    /// Aborts the task and waits until it is gone: an abort only takes effect when the runtime
    /// next polls the task, and what the task holds (a store handle) is released then.
    pub(super) async fn stop(&mut self) {
        self.0.abort();
        let _ = (&mut self.0).await;
    }
}

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// The tasks that keep running beside the workspaces, each stopped when it drops.
pub(super) struct Watchers {
    pub(super) retired_sweep: tokio::task::JoinHandle<()>,
    pub(super) git_changes: AbortOnDrop,
    pub(super) sign_in_notices: AbortOnDrop,
    pub(super) editor_updates: AbortOnDrop,
    pub(super) space_watch: AbortOnDrop,
    pub(super) disk_watch: AbortOnDrop,
}

pub(super) fn spawn_watchers<R: Runtime + Clone>(
    shared: &Shared<'_>,
    service: &HostWorkspaces<R>,
    injecting: &Injecting,
    editor_windows: &puddle_front::Windows,
) -> Watchers {
    // Downloads the image the delete check boots now, while nobody is waiting on it.
    service.warm_maintenance_image_for_listed();
    // Finishes what a delete that did not end left under its id.
    let retired_sweep = tokio::spawn({
        let service = service.clone();
        async move { service.sweep_retired().await }
    });
    let (git_changes, sign_in_notices) = injecting.watch(service, shared.events);
    let editor_updates = AbortOnDrop(crate::editor_updates::spawn(
        service.clone(),
        editor_windows.clone(),
    ));
    let space_watch = AbortOnDrop(crate::space_watch::spawn(
        crate::space_watch::space_or_drive(
            shared.options.free_space.clone(),
            shared.config.paths.data(),
        ),
        shared.config.free_space,
        shared.settings.clone(),
        shared.events.clone(),
        Arc::new({
            let service = service.clone();
            move |level| service.note_host_disk(level)
        }),
    ));
    let disk_watch = AbortOnDrop(crate::disk_watch::spawn(service.clone()));
    Watchers {
        retired_sweep,
        git_changes,
        sign_in_notices,
        editor_updates,
        space_watch,
        disk_watch,
    }
}
