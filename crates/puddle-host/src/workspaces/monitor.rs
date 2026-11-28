// SPDX-License-Identifier: AGPL-3.0-or-later
//! Disk use, dead VMs and what a session end triggers.

use std::sync::PoisonError;

use puddle_compute::Runtime;
use puddle_services::workspaces::WorkspaceRecord;
use puddle_services::workspaces::phase::Move;
use puddle_types::{DiskKind, Event, EventSink, Problem, WorkspaceId, WorkspaceStatus};

use super::{HostWorkspaces, SESSION_END_GAP, SESSION_END_READS, disk_used};

/// Key prefix of the notice that the host drive is full, one per workspace.
const HOST_DISK_FULL: &str = "host-disk-full:";

impl<R: Runtime + Clone> HostWorkspaces<R> {
    /// Reads what every workspace's volume and root disk take on the host into the records, so
    /// the list and the single-workspace answer both carry it, then warns about disks that
    /// reached the threshold. A runtime that cannot say leaves the earlier value; the card shows
    /// the size alone until it can.
    pub(crate) async fn refresh_disk_use(&self) {
        match self.inner.runtime.list_volumes().await {
            Ok(volumes) => {
                for slot in self.state().slots.values_mut() {
                    slot.record.disk_used_mib = disk_used(&volumes, &slot.record.id);
                }
            }
            Err(e) => tracing::debug!(error = %e, "the volumes' use of the disk is unknown"),
        }
        match self.inner.runtime.list().await {
            Ok(sandboxes) => {
                self.notice_dead_vms(&sandboxes).await;
                for slot in self.state().slots.values_mut() {
                    let root = sandboxes
                        .iter()
                        .find(|s| s.name == slot.record.id.sandbox_name().as_str())
                        .and_then(|s| s.root_disk);
                    slot.record.root_disk_size_mib = root.map(|r| u64::from(r.size.as_mib()));
                    slot.record.root_disk_used_mib = root.map(|r| u64::from(r.used.as_mib()));
                }
            }
            Err(e) => tracing::debug!(error = %e, "the root disks' use of the disk is unknown"),
        }
        self.warn_about_full_disks();
    }

    /// Tells the user, per running workspace, that the drive holding Puddle's data is full, and
    /// takes the notice back when space returns. A full drive makes a workspace's writes fail
    /// with an input/output error while it still shows Running, so the notice says why.
    pub(crate) fn note_host_disk(&self, level: puddle_types::DiskSpaceLevel) {
        if level != puddle_types::DiskSpaceLevel::Critical {
            self.inner.problems.clear_prefix(HOST_DISK_FULL);
            return;
        }
        let running: Vec<WorkspaceId> = self
            .state()
            .slots
            .values()
            .filter(|s| s.record.status == WorkspaceStatus::Running)
            .map(|s| s.record.id.clone())
            .collect();
        for id in running {
            self.inner.problems.raise(
                Problem::new(
                    format!("{HOST_DISK_FULL}{id}"),
                    format!(
                        "{name} cannot save files: the disk Puddle stores workspaces on is full",
                        name = self.label(&id)
                    ),
                    "Writes inside the workspace fail with an input/output error until space is \
                     freed on that drive. Delete files outside Puddle, or stop other workspaces \
                     and use Reclaim space. This notice ends when enough space is free again.",
                )
                .about(id),
            );
        }
    }

    /// Reads the sandboxes again for a while after `workspace`'s agent session ended, until the
    /// workspace stops showing Running. A VM that died takes a moment to be reported down; one
    /// that did not (the agent restarted) is simply still up after the last read.
    pub(crate) async fn check_after_session_end(&self, workspace: &WorkspaceId) {
        for _ in 0..SESSION_END_READS {
            tokio::time::sleep(SESSION_END_GAP).await;
            if self.status_of(workspace) != Some(WorkspaceStatus::Running) {
                return;
            }
            self.refresh_disk_use().await;
        }
    }

    pub(super) fn status_of(&self, workspace: &WorkspaceId) -> Option<WorkspaceStatus> {
        self.state().slots.get(workspace).map(|s| s.record.status)
    }

    /// Moves every workspace that shows Running while its sandbox is down to Crashed, and lets go
    /// of what the dead VM held (terminals, forwards, the CA, the SSH endpoint) so a start works.
    /// A workspace with an operation in progress is left to it; the operation's end decides.
    pub(crate) async fn notice_dead_vms(&self, sandboxes: &[puddle_compute::SandboxInfo]) {
        let dead: Vec<WorkspaceRecord> = {
            let mut state = self.state();
            state
                .slots
                .values_mut()
                .filter(|slot| {
                    slot.record.status == WorkspaceStatus::Running && slot.record.busy.is_none()
                })
                .filter(|slot| {
                    let name = slot.record.id.sandbox_name();
                    sandboxes
                        .iter()
                        .any(|s| s.name == name.as_str() && s.status.is_down())
                })
                .map(|slot| {
                    // Running, idle: the transition is to crashed.
                    let _ = slot.record.apply(Move::VmDied);
                    slot.record.clone()
                })
                .collect()
        };
        for record in dead {
            tracing::warn!(workspace = %record.id, "the VM ended while Puddle ran; the workspace is crashed");
            self.status_event(&record);
            self.quiesce(&record.id, false).await;
            self.inner.reports.end(&record.id);
        }
    }

    /// Says once, per disk, that it reached the warning threshold, and again only after it was
    /// below it. A threshold that cannot be read is Puddle's default.
    pub(super) fn warn_about_full_disks(&self) {
        let threshold = crate::settings_read::global(self.inner.settings.as_ref())
            .map(|g| g.ui.disk_warning_percent())
            .unwrap_or_default();
        let records: Vec<WorkspaceRecord> = self
            .state()
            .slots
            .values()
            .map(|s| s.record.clone())
            .collect();
        let mut warned = self
            .inner
            .disk_warned
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        for record in records {
            let over = record.disk_warnings(threshold);
            for disk in [DiskKind::Root, DiskKind::Volume] {
                let key = (record.id.clone(), disk);
                if !over.contains(&disk) {
                    warned.remove(&key);
                } else if warned.insert(key)
                    && let Some(percent) = record.disk_percent(disk)
                {
                    self.inner.events.emit(Event::DiskNearlyFull {
                        workspace: record.id.clone(),
                        disk,
                        percent,
                    });
                }
            }
        }
    }
}
