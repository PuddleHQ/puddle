// SPDX-License-Identifier: GPL-3.0-or-later
//! Creating a workspace and the root disk work.

use std::collections::BTreeSet;
use std::sync::Arc;

use puddle_api::WorkspaceRecord;
use puddle_api::{EndReason, TerminalError};
use puddle_boot::GatedSandbox;
use puddle_compute::{DiskSize, RootDiskGrowth, Runtime};
use puddle_store::{WorkspaceOpKind, WorkspaceOpOutcome};
use puddle_types::{
    CheckoutDir, ImageRef, Problem, SessionUser, WorkspaceId, WorkspaceStatus, WorkspaceStep,
};

use puddle_workspace::Import;

use super::{
    Grown, HostWorkspaces, Room, TERMINAL_BOOT_POLL, TERMINAL_BOOT_WAIT, clone_problem_key,
    problem_key, with_leftovers,
};
use crate::op_error::OpError;

impl<R: Runtime + Clone> HostWorkspaces<R> {
    pub(super) async fn create_work(
        &self,
        record: WorkspaceRecord,
        imports: Vec<Import>,
    ) -> Result<(), OpError> {
        self.inner.reports.begin(&record.id, &record.name);
        let (imported, mut created_volume) = self.import_all(&record, &imports).await?;
        let mut created_by_boot = false;
        if let Err(reason) = self
            .boot_new(&record, &mut created_by_boot, true, true)
            .await
        {
            return Err(if created_volume {
                self.remove_new_volume(&record, reason).await
            } else {
                reason
            });
        }
        created_volume |= created_by_boot;
        if let Err(reason) = self.settle(&record).await {
            return Err(self.undo_boot(&record, true, created_volume, reason).await);
        }
        let gated = self.gated(&record.id).await?;
        if let Err(reason) = self.clone_all(&record, gated.ungated(), &imported).await {
            return Err(self.undo_boot(&record, true, created_volume, reason).await);
        }
        // Nobody has worked in it yet, so undoing it now loses nothing; keeping it would leave
        // a workspace the next start takes for an unfinished create.
        if let Err(reason) = self.mark_created(&record.id) {
            return Err(self.undo_boot(&record, true, created_volume, reason).await);
        }
        self.refresh_checkouts_later(&record.id);
        Ok(())
    }

    /// Clones the checkouts on this computer of a new workspace onto its volume, before its
    /// sandbox boots (see [`Workspaces::import`]). Returns the directories they went to (cloned or
    /// refused: neither is cloned again from its address) and whether this made the volume. A
    /// checkout git refuses fails only its repository, as a clone does.
    pub(super) async fn import_all(
        &self,
        record: &WorkspaceRecord,
        imports: &[Import],
    ) -> Result<(BTreeSet<CheckoutDir>, bool), OpError> {
        if imports.is_empty() {
            return Ok((BTreeSet::new(), false));
        }
        let image = ImageRef::new(&record.image)?;
        let size = DiskSize::mib(u32::try_from(record.disk_size_mib).unwrap_or(u32::MAX));
        for import in imports {
            if let Some(repository) = record
                .repositories
                .iter()
                .find(|r| r.directory == import.directory)
            {
                self.progress_cloning(&record.id, repository);
            }
        }
        let report = self
            .inner
            .workspaces
            .import(&self.inner.runtime, &record.id, &image, size, imports)
            .await?;
        for (directory, reason) in &report.failed {
            if let Some(repository) = record
                .repositories
                .iter()
                .find(|r| &r.directory == directory)
            {
                tracing::warn!(workspace = %record.id, checkout = %directory, %reason, "a checkout of a new workspace was not imported");
                let reason = format!(
                    "{reason}. To try again, add the repository by its address on the Git tab"
                );
                self.raise_clone_problem(record, repository, &reason, true);
            }
        }
        let done = imports.iter().map(|i| i.directory.clone()).collect();
        Ok((done, report.created_volume))
    }

    /// Removes the volume a create made before its boot failed, and says so if that failed too.
    pub(super) async fn remove_new_volume(
        &self,
        record: &WorkspaceRecord,
        reason: OpError,
    ) -> OpError {
        match self
            .inner
            .runtime
            .remove_volume(&record.id.volume_name())
            .await
        {
            Ok(()) => reason,
            Err(e) => {
                tracing::warn!(workspace = %record.id, error = %e, "removing a new volume failed");
                with_leftovers(
                    reason,
                    &[format!(
                        "the new volume {} was not removed ({e})",
                        record.id.volume_name()
                    )],
                )
            }
        }
    }

    /// Clones the repositories of a new workspace one at a time in list order, then runs one
    /// `sync`. A clone git refuses fails only its repository: a background problem names it and
    /// the next one starts (git removes the folder of its own failed clone). Anything else (the
    /// guest gone, the sync failing) fails the create, which nobody has worked in yet.
    pub(super) async fn clone_all(
        &self,
        record: &WorkspaceRecord,
        sandbox: &R::Sandbox,
        imported: &BTreeSet<CheckoutDir>,
    ) -> Result<(), OpError> {
        let workspaces = &self.inner.workspaces;
        let mut any_cloned = false;
        for repository in &record.repositories {
            if imported.contains(&repository.directory) {
                continue;
            }
            any_cloned = true;
            self.progress_cloning(&record.id, repository);
            let key = clone_problem_key(&record.id, &repository.entry);
            let cloned = workspaces
                .clone_checkout(
                    sandbox,
                    &record.id,
                    repository.url.as_str(),
                    &repository.directory,
                    self.booted_user(&record.id).await,
                )
                .await;
            match cloned {
                Ok(_) => self.inner.problems.clear(&key),
                Err(puddle_workspace::WorkspaceError::Clone { reason, .. }) => {
                    tracing::warn!(workspace = %record.id, url = %repository.url, %reason, "a repository of a new workspace was not cloned");
                    self.raise_clone_problem(record, repository, &reason, true);
                }
                Err(other) => return Err(other.into()),
            }
        }
        if !any_cloned {
            return Ok(());
        }
        Ok(workspaces.sync(sandbox, &record.id).await?)
    }

    /// The user the running guest of `name` started with, which its clones and checks run as
    /// until the next start whatever the setting says now; root when it does not run.
    pub(super) async fn booted_user(&self, name: &WorkspaceId) -> SessionUser {
        self.inner
            .live
            .lock()
            .await
            .get(name)
            .and_then(|l| l.guest.as_ref())
            .map_or(SessionUser::Root, |g| g.session_user)
    }

    pub(super) async fn gated(
        &self,
        name: &WorkspaceId,
    ) -> Result<Arc<GatedSandbox<R::Sandbox>>, OpError> {
        self.inner
            .live
            .lock()
            .await
            .get(name)
            .and_then(|l| l.gated.clone())
            .ok_or_else(|| OpError::NotRunning(name.clone()))
    }

    /// Why a terminal's connection that ended without an exit status did: the workspace crashed,
    /// or it stopped (or went away).
    pub(super) fn terminal_gone(&self, name: &WorkspaceId) -> EndReason {
        crate::terminals::reason_for(self.state().slots.get(name).map(|s| s.record.status))
    }

    /// The running guest's sandbox and the user its sessions run as, once the workspace is
    /// running; a workspace that is starting is waited for up to [`TERMINAL_BOOT_WAIT`].
    pub(super) async fn terminal_target(
        &self,
        name: &WorkspaceId,
    ) -> Result<(Arc<GatedSandbox<R::Sandbox>>, SessionUser), TerminalError> {
        crate::terminals::wait_until_ready(TERMINAL_BOOT_WAIT, TERMINAL_BOOT_POLL, || async {
            let record = self.state().slots.get(name).map(|s| s.record.clone());
            let Some(record) = record.filter(|r| {
                matches!(
                    r.status,
                    WorkspaceStatus::Running | WorkspaceStatus::Starting
                )
            }) else {
                return Err(TerminalError::NotRunning(
                    "the workspace is not running".into(),
                ));
            };
            let live = self.inner.live.lock().await;
            Ok(live.get(name).and_then(|l| {
                let user = l
                    .guest
                    .as_ref()
                    .map_or(record.session_user, |g| g.session_user);
                Some((l.gated.clone()?, user))
            }))
        })
        .await
    }

    /// Removes the workspace's sandbox, if it has one: the root disk goes with it, the volume
    /// stays, and the next start makes both again from the image.
    pub(super) async fn reset_root_disk_work(&self, name: &WorkspaceId) -> Result<(), OpError> {
        let sandbox = name.sandbox_name();
        let exists = self
            .inner
            .runtime
            .list()
            .await?
            .iter()
            .any(|s| s.name == sandbox.as_str());
        if exists {
            self.inner.runtime.remove(&sandbox).await?;
            self.inner.workspaces.sandbox_removed(&sandbox);
        }
        self.inner.problems.clear(&problem_key("root-copy", name));
        // A route left from an earlier boot belongs to a sandbox that no longer exists.
        self.quiesce(name, true).await;
        Ok(())
    }

    /// Sets the workspace's status and tells the windows.
    pub(super) fn set_status(&self, id: &WorkspaceId, status: WorkspaceStatus) {
        let record = {
            let mut state = self.state();
            let Some(slot) = state.slots.get_mut(id) else {
                return;
            };
            slot.record.status = status;
            slot.record.clone()
        };
        self.status_event(&record);
    }

    /// Makes the workspace's root disk `size` big and writes what came of it into the audit log.
    /// `from_mib` is the size it had; `room` how the added room compares with the free space.
    pub(super) async fn grow_root_disk_work(
        &self,
        record: WorkspaceRecord,
        from_mib: u64,
        size: DiskSize,
        room: Room,
    ) -> Result<(), OpError> {
        let result = self.grow_root_disk_steps(&record, size).await;
        // The grown size is saved and the runtime has accepted it, so the grow is done even when
        // the restart that applies it failed; that failure is reported on its own.
        let (outcome, detail) = match &result {
            Ok(grown) => (
                WorkspaceOpOutcome::Succeeded,
                match &grown.restart_failure {
                    None => format!(
                        "root disk grown from {from_mib} MiB to {} MiB, {}",
                        size.as_mib(),
                        grown.how
                    ),
                    Some(reason) => format!(
                        "root disk grown from {from_mib} MiB to {} MiB, {}; the restart failed: \
                         {reason}",
                        size.as_mib(),
                        grown.how
                    ),
                },
            ),
            Err(reason) => (
                WorkspaceOpOutcome::Failed,
                format!(
                    "root disk not grown from {from_mib} MiB to {} MiB: {reason}",
                    size.as_mib()
                ),
            ),
        };
        self.record_op(
            WorkspaceOpKind::RootDiskGrown,
            &record.id,
            outcome,
            Some(detail),
        );
        let key = problem_key("root-room", &record.id);
        if result.is_ok() && room == Room::Tight {
            self.inner.problems.raise(
                Problem::new(
                    key,
                    format!(
                        "{}'s root disk is bigger than most of the free space left",
                        record.name
                    ),
                    format!(
                        "Its root disk can now hold {} GiB. The file only takes room on this \
                         computer as the workspace writes to it, so nothing more is used yet, \
                         but a workspace that fills it would use most of what is free.",
                        size.as_mib() / 1024
                    ),
                )
                .about(record.id.clone()),
            );
        } else {
            self.inner.problems.clear(&key);
        }
        result.and_then(|grown| grown.restart_failure.map_or(Ok(()), Err))
    }

    /// The grow itself; on success says how it took effect, for the audit entry.
    pub(super) async fn grow_root_disk_steps(
        &self,
        record: &WorkspaceRecord,
        size: DiskSize,
    ) -> Result<Grown, OpError> {
        let inner = &self.inner;
        let name = &record.id;
        self.progress(name, WorkspaceStep::GrowingRootDisk, None);
        let sandbox = name.sandbox_name();
        let exists = inner
            .runtime
            .list()
            .await?
            .iter()
            .any(|s| s.name == sandbox.as_str());
        let growth = if exists {
            Some(inner.runtime.grow_root_disk(&sandbox, size).await?)
        } else {
            None
        };
        // Saved only now: the size is a promise that the disk has the room (or will at its next
        // start), so a refused grow leaves the old one.
        {
            let mut state = self.state();
            if let Some(slot) = state.slots.get_mut(name) {
                slot.record.root_disk_grown_mib = Some(size.as_mib());
            }
            self.persist(&state)
                .map_err(|source| OpError::SaveFailed { source })?;
        }
        match growth {
            None => Ok(Grown::done("for the first start")),
            Some(RootDiskGrowth::Live) => Ok(Grown::done("while it ran")),
            Some(RootDiskGrowth::NextStart) if record.status == WorkspaceStatus::Running => {
                self.progress(
                    name,
                    WorkspaceStep::GrowingRootDisk,
                    Some(
                        "this computer cannot make a running disk bigger, so the workspace is \
                         restarted; what was saved to disk stays"
                            .to_owned(),
                    ),
                );
                self.set_status(name, WorkspaceStatus::Draining);
                inner.reports.pause(name);
                let restarted = async {
                    self.stop_work(record.clone()).await?;
                    self.set_status(name, WorkspaceStatus::Starting);
                    self.start_work(record.clone()).await
                }
                .await;
                Ok(Grown {
                    how: "with a restart",
                    restart_failure: restarted.err(),
                })
            }
            Some(RootDiskGrowth::NextStart) => {
                inner.problems.raise(
                    Problem::new(
                        problem_key("root-copy", name),
                        format!(
                            "{}'s old root disk file is still on this computer",
                            record.name
                        ),
                        "Growing a stopped workspace's root disk makes a bigger copy of the file \
                         and leaves the old one behind, so for now the workspace takes room on \
                         this computer twice. Resetting the root disk or deleting the \
                         workspace frees it."
                            .to_owned(),
                    )
                    .about(name.clone()),
                );
                Ok(Grown::done("at its next start"))
            }
        }
    }
}
