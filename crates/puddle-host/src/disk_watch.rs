// SPDX-License-Identifier: GPL-3.0-or-later
//! Watches how full the workspaces' disks are while nobody asks.
//!
//! The list and every finished operation already read the disks' use; a root disk or volume that
//! fills during a long run changes none of those, so the host reads it again now and then and
//! warns when a disk crosses the threshold.

use std::time::Duration;

use puddle_compute::Runtime;

use crate::workspaces::HostWorkspaces;

/// How often the host reads the disks. Filling a disk takes minutes at the least, and a read
/// stats a file or two per workspace.
pub(crate) const READ_PERIOD: Duration = Duration::from_mins(1);

/// Reads every [`READ_PERIOD`] until the returned task is aborted.
pub(crate) fn spawn<R: Runtime + Clone>(
    workspaces: HostWorkspaces<R>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(READ_PERIOD);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        tick.tick().await;
        loop {
            tick.tick().await;
            workspaces.refresh_disk_use().await;
        }
    })
}
