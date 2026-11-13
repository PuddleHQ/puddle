// SPDX-License-Identifier: GPL-3.0-or-later
//! The `TerminalService` and `WorkspaceService` implementations of the host.

use std::sync::Arc;

use futures_util::future::BoxFuture;
use puddle_api::{
    AttachMode, Attached, DEFAULT_IMAGE, DeleteCheck, DisplayName, GitSetup, KeptLogins,
    LocalCheckoutError, LocalSource, NewWorkspace, Operation, ShellInitCheck, StartFolder,
    StartFolderChange, WorkspaceError, WorkspaceRecord, WorkspaceService,
};
use puddle_api::{TerminalError, TerminalLink, TerminalService, TerminalSize};
use puddle_compute::{DiskSize, Runtime};
use puddle_store::{WorkspaceOpKind, WorkspaceOpOutcome};
use puddle_types::{EntryId, SessionUser, WorkspaceId, WorkspaceStatus, WorkspaceStep};

use puddle_workspace::{Import, LocalCheckout};

use super::{
    HostWorkspaces, Room, SAVE_HINT, Slot, checkout_refusal, delete_check, kept_login_row,
    not_found, problem_key, room, workspace_error,
};
use crate::boot::root_disk_size;
use crate::op_error::OpError;

impl<R: Runtime + Clone> TerminalService for HostWorkspaces<R> {
    fn check(&self, workspace: &WorkspaceId) -> BoxFuture<'_, Result<(), TerminalError>> {
        let running = self.state().slots.get(workspace).is_some_and(|s| {
            matches!(
                s.record.status,
                WorkspaceStatus::Running | WorkspaceStatus::Starting
            )
        });
        Box::pin(async move {
            if running {
                Ok(())
            } else {
                Err(TerminalError::NotRunning(
                    "the workspace is not running; start it to open a terminal".into(),
                ))
            }
        })
    }

    fn open(
        &self,
        workspace: &WorkspaceId,
        size: TerminalSize,
    ) -> BoxFuture<'_, Result<TerminalLink, TerminalError>> {
        let workspace = workspace.clone();
        Box::pin(async move {
            let identity = self
                .inner
                .ssh
                .identity()
                .map_err(|e| TerminalError::Unavailable(e.to_string()))?;
            let (gated, user) = self.terminal_target(&workspace).await?;
            let host = self.clone();
            let named = workspace.clone();
            crate::terminals::open(
                &self.inner.terminals,
                crate::terminals::Open {
                    workspace: &workspace,
                    identity: &identity,
                    user: &user.to_string(),
                    size,
                },
                move |stream| async move { gated.serve_ssh(stream).await },
                move || host.terminal_gone(&named),
            )
            .await
        })
    }
}

impl<R: Runtime + Clone> WorkspaceService for HostWorkspaces<R> {
    fn local_checkout(&self, path: &str) -> Result<LocalSource, LocalCheckoutError> {
        let found =
            LocalCheckout::open(std::path::Path::new(path)).map_err(LocalCheckoutError::Refused)?;
        // The runtime's own rule (owner, Puddle's folders) now, not only when the create runs.
        self.inner
            .runtime
            .check_checkout(&found.git_dir)
            .map_err(checkout_refusal)?;
        Ok(LocalSource {
            folder_name: found
                .folder
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            git_dir: found.git_dir,
            origin: found.origin,
        })
    }

    fn list(&self) -> BoxFuture<'_, Result<Vec<WorkspaceRecord>, WorkspaceError>> {
        Box::pin(async move {
            self.refresh_disk_use().await;
            Ok(self
                .state()
                .slots
                .values()
                .map(|s| s.record.clone())
                .collect())
        })
    }

    fn get<'a>(
        &'a self,
        id: &'a WorkspaceId,
    ) -> BoxFuture<'a, Result<WorkspaceRecord, WorkspaceError>> {
        Box::pin(async move {
            self.state()
                .slots
                .get(id)
                .map(|s| s.record.clone())
                .ok_or_else(|| not_found(id))
        })
    }

    fn create<'a>(
        &'a self,
        new: NewWorkspace,
        git: &'a dyn GitSetup,
    ) -> BoxFuture<'a, Result<WorkspaceRecord, WorkspaceError>> {
        Box::pin(async move {
            let runtime_names = self.runtime_names().await?;
            let memory = match new.memory {
                Some(memory) => memory,
                None => self.default_memory().map_err(|reason| {
                    WorkspaceError::Conflict(format!(
                        "{reason}, so Puddle can't tell how much memory the new workspace gets; \
                         fix or reset the settings, or choose a memory size for it"
                    ))
                })?,
            };
            let ticket = {
                let mut state = self.state();
                if state.closed {
                    return Err(WorkspaceError::Unavailable(
                        "Puddle is shutting down".to_owned(),
                    ));
                }
                if puddle_api::label_taken(
                    state.slots.values().map(|s| &s.record),
                    new.name.as_str(),
                    None,
                )
                .is_some()
                {
                    return Err(WorkspaceError::Conflict(format!(
                        "a workspace named {} already exists",
                        new.name
                    )));
                }
                // Drawn and reserved under one lock, so two creates never get the same id.
                let id = new
                    .id(&*self.inner.ids, |id| {
                        self.id_taken(&state, &runtime_names, id)
                    })
                    .map_err(WorkspaceError::unavailable)?;
                // Whose it is, before it is listed or announced.
                git.claim(&id, new.owner)?;
                let mut record = WorkspaceRecord::new(id.clone(), new.name.clone(), new.entries());
                record.start_folder.clone_from(&new.start_folder);
                record.image = new
                    .image
                    .as_ref()
                    .map_or_else(|| DEFAULT_IMAGE.to_owned(), |i| i.as_str().to_owned());
                record.memory = memory;
                record.created_at = self.inner.clock.now_ms();
                record.disk_size_mib =
                    u64::from(self.inner.workspaces.config().default_size.as_mib());
                record.disk_used_mib = None;
                record.busy = Some(Operation::Creating);
                state
                    .slots
                    .insert(id.clone(), Slot::new(record.clone(), true));
                self.ticket(record, Operation::Creating)
            };
            let record = ticket.record().clone();
            let id = record.id.clone();
            // A second guard: a drawn id is never one with browser data left (`id_taken`), so
            // this finds nothing unless a window wrote some since. Reserved first, so no other
            // create can be clearing the same id.
            self.clear_left_over_browser_state(&id, &new.name).await?;
            {
                let mut state = self.state();
                if let Err(e) = self.persist(&state) {
                    state.slots.remove(&id);
                    return Err(WorkspaceError::Unavailable(format!(
                        "{} was not created: {e}; {SAVE_HINT}",
                        new.name
                    )));
                }
            }
            // The name is reserved and the list saved: set up what the clones need first.
            git.set_up(&record.id, &new.repositories).await;
            self.status_event(&record);
            let imports: Vec<Import> = new
                .repositories
                .iter()
                .filter_map(|r| {
                    r.local.as_ref().map(|git_dir| Import {
                        git_dir: git_dir.clone(),
                        directory: r.repository.directory.clone(),
                        url: r.repository.url.as_str().to_owned(),
                    })
                })
                .collect();
            let this = self.clone();
            let work = record.clone();
            self.run(ticket, async move { this.create_work(work, imports).await });
            Ok(record)
        })
    }

    fn start<'a>(
        &'a self,
        id: &'a WorkspaceId,
    ) -> BoxFuture<'a, Result<WorkspaceRecord, WorkspaceError>> {
        Box::pin(async move {
            let ticket = self.accept(
                id,
                Operation::Starting,
                |r| {
                    if r.status.is_down() {
                        Ok(())
                    } else {
                        Err(WorkspaceError::Conflict(format!(
                            "{} is already {}",
                            r.name, r.status
                        )))
                    }
                },
                |r| r.status = WorkspaceStatus::Starting,
            )?;
            let record = ticket.record().clone();
            self.status_event(&record);
            let this = self.clone();
            let work = record.clone();
            self.run(ticket, async move { this.start_work(work).await });
            Ok(record)
        })
    }

    fn stop<'a>(
        &'a self,
        id: &'a WorkspaceId,
    ) -> BoxFuture<'a, Result<WorkspaceRecord, WorkspaceError>> {
        Box::pin(async move {
            let ticket = self.accept(
                id,
                Operation::Stopping,
                |r| {
                    if r.status == WorkspaceStatus::Running {
                        Ok(())
                    } else {
                        Err(WorkspaceError::Conflict(format!(
                            "{} is not running ({})",
                            r.name, r.status
                        )))
                    }
                },
                |r| r.status = WorkspaceStatus::Draining,
            )?;
            let record = ticket.record().clone();
            self.status_event(&record);
            // A stopped workspace is the one a user deletes next: have the delete check's image
            // ready by then.
            self.warm_maintenance_image();
            let this = self.clone();
            let work = record.clone();
            self.run(ticket, async move {
                this.inner.reports.pause(&work.id);
                this.stop_work(work).await
            });
            Ok(record)
        })
    }

    fn reclaim<'a>(
        &'a self,
        id: &'a WorkspaceId,
    ) -> BoxFuture<'a, Result<WorkspaceRecord, WorkspaceError>> {
        Box::pin(async move {
            let ticket = self.accept(
                id,
                Operation::Reclaiming,
                |r| {
                    if matches!(
                        r.status,
                        WorkspaceStatus::Starting | WorkspaceStatus::Draining
                    ) {
                        Err(WorkspaceError::Conflict(format!(
                            "{} is {}; try again in a moment",
                            r.name, r.status
                        )))
                    } else {
                        Ok(())
                    }
                },
                |_| {},
            )?;
            let record = ticket.record().clone();
            let this = self.clone();
            let work = record.clone();
            self.run(ticket, async move {
                this.progress(&work.id, WorkspaceStep::Reclaiming, None);
                this.inner
                    .workspaces
                    .reclaim_space(&this.inner.runtime, &work.id)
                    .await
                    .map(|_| ())
                    .map_err(OpError::from)
            });
            Ok(record)
        })
    }

    fn reset_root_disk<'a>(
        &'a self,
        id: &'a WorkspaceId,
    ) -> BoxFuture<'a, Result<WorkspaceRecord, WorkspaceError>> {
        Box::pin(async move {
            let ticket = self.accept(
                id,
                Operation::ResettingRootDisk,
                |r| {
                    if r.status.is_down() {
                        Ok(())
                    } else {
                        Err(WorkspaceError::Conflict(format!(
                            "{} is {}; stop it before resetting its root disk",
                            r.name, r.status
                        )))
                    }
                },
                |_| {},
            )?;
            let record = ticket.record().clone();
            let this = self.clone();
            let work = record.clone();
            self.run(ticket, async move {
                this.progress(&work.id, WorkspaceStep::ResettingRootDisk, None);
                this.reset_root_disk_work(&work.id).await
            });
            Ok(record)
        })
    }

    fn grow_root_disk<'a>(
        &'a self,
        id: &'a WorkspaceId,
        size_mib: u32,
    ) -> BoxFuture<'a, Result<WorkspaceRecord, WorkspaceError>> {
        Box::pin(async move {
            let size = DiskSize::mib(size_mib);
            let current = {
                let state = self.state();
                let slot = state.slots.get(id).ok_or_else(|| not_found(id))?;
                // The reported size lags a grow that applies at the next start; the saved one
                // does not, so the larger of the two is what a request has to beat.
                let saved = u64::from(root_disk_size(slot.record.root_disk_grown_mib).as_mib());
                slot.record
                    .root_disk_size_mib
                    .map_or(saved, |r| r.max(saved))
            };
            if u64::from(size_mib) <= current {
                return Err(WorkspaceError::Invalid(format!(
                    "a root disk only grows: {size_mib} MiB is not above its {current} MiB"
                )));
            }
            let added = (u64::from(size_mib) - current) * 1024 * 1024;
            let space = self.inner.space.clone();
            let free = tokio::task::spawn_blocking(move || space.free_bytes())
                .await
                .ok()
                .and_then(Result::ok);
            let room = match free {
                Some(free) => room(added, free),
                None => Room::Plenty,
            };
            if room == Room::TooBig {
                return Err(WorkspaceError::Invalid(format!(
                    "this computer has {} GiB free, which is less than the {} GiB this adds to \
                     the root disk; choose a smaller size or free some space",
                    free.unwrap_or(0) / (1024 * 1024 * 1024),
                    added / (1024 * 1024 * 1024)
                )));
            }
            let ticket = self.accept(
                id,
                Operation::GrowingRootDisk,
                |r| {
                    if matches!(
                        r.status,
                        WorkspaceStatus::Starting | WorkspaceStatus::Draining
                    ) {
                        Err(WorkspaceError::Conflict(format!(
                            "{} is {}; try again in a moment",
                            r.name, r.status
                        )))
                    } else {
                        Ok(())
                    }
                },
                |_| {},
            )?;
            let record = ticket.record().clone();
            let this = self.clone();
            let work = record.clone();
            self.run(ticket, async move {
                this.grow_root_disk_work(work, current, size, room).await
            });
            Ok(record)
        })
    }

    fn delete_check<'a>(
        &'a self,
        id: &'a WorkspaceId,
    ) -> BoxFuture<'a, Result<DeleteCheck, WorkspaceError>> {
        Box::pin(async move {
            if !self.state().slots.contains_key(id) {
                return Err(not_found(id));
            }
            let (this, target) = (self.clone(), id.clone());
            let report = self
                .maintenance_job(async move {
                    this.inner
                        .workspaces
                        .check_delete(&this.inner.runtime, &target)
                        .await
                })
                .await?
                .map_err(|e| workspace_error(&e))?;
            Ok(delete_check(&report))
        })
    }

    fn delete<'a>(
        &'a self,
        id: &'a WorkspaceId,
        fingerprint: Option<&'a str>,
    ) -> BoxFuture<'a, Result<WorkspaceRecord, WorkspaceError>> {
        Box::pin(async move {
            let ticket = self.accept(
                id,
                Operation::Deleting,
                |r| {
                    if r.status.is_down() {
                        Ok(())
                    } else {
                        Err(WorkspaceError::Conflict(format!(
                            "{} is {}; stop it before deleting it",
                            r.name, r.status
                        )))
                    }
                },
                |_| {},
            )?;
            let record = ticket.record().clone();
            // The check runs before the call returns, so a refusal reaches the caller.
            // The ticket goes with the check: a request dropped while it runs leaves the
            // workspace busy until the check's sandbox is gone.
            let (this, target) = (self.clone(), id.clone());
            let (ticket, checked) = self
                .maintenance_job(async move {
                    let checked = this
                        .inner
                        .workspaces
                        .check_delete(&this.inner.runtime, &target)
                        .await;
                    (ticket, checked)
                })
                .await?;
            let verdict = match checked {
                Err(e) => Err(workspace_error(&e)),
                Ok(report) => {
                    let fresh = delete_check(&report);
                    match fingerprint {
                        Some(seen) if seen == fresh.fingerprint => Ok(report),
                        Some(_) => Err(WorkspaceError::Conflict(format!(
                            "{} has changed since it was checked; check it again",
                            record.name
                        ))),
                        None if fresh.is_clean() => Ok(report),
                        None => Err(WorkspaceError::Conflict(format!(
                            "{} has work that is not saved on a remote; review it and confirm",
                            record.name
                        ))),
                    }
                }
            };
            // The entry leaves the saved list before the volume goes, so a delete that can't be
            // saved deletes nothing.
            let verdict = verdict.and_then(|report| {
                let mut state = self.state();
                if let Some(slot) = state.slots.get_mut(id) {
                    slot.deleting = true;
                }
                if report.volume_missing
                    && let Some(slot) = state.slots.get_mut(id)
                {
                    slot.delete_note = Some("deleted; its volume was already gone");
                }
                match self.persist(&state) {
                    Ok(()) => Ok(report),
                    Err(e) => Err(WorkspaceError::Unavailable(format!(
                        "{} was not deleted: {e}; {SAVE_HINT}",
                        record.name
                    ))),
                }
            });
            let report = match verdict {
                Ok(report) => report,
                Err(e) => return Err(e),
            };
            let this = self.clone();
            let work = record.clone();
            self.run(ticket, async move { this.delete_work(work, report).await });
            Ok(record)
        })
    }

    fn rename<'a>(
        &'a self,
        id: &'a WorkspaceId,
        name: DisplayName,
    ) -> BoxFuture<'a, Result<WorkspaceRecord, WorkspaceError>> {
        Box::pin(async move {
            let mut state = self.state();
            if state.closed {
                return Err(WorkspaceError::Unavailable(
                    "Puddle is shutting down".to_owned(),
                ));
            }
            puddle_api::check_rename(state.slots.values().map(|s| &s.record), id, &name)?;
            let slot = state.slots.get_mut(id).ok_or_else(|| not_found(id))?;
            let before = std::mem::replace(&mut slot.record.name, name);
            if let Err(e) = self.persist(&state) {
                // Nothing changes when the list can't be saved, so a restart doesn't undo a
                // rename the user was told about.
                if let Some(slot) = state.slots.get_mut(id) {
                    slot.record.name = before;
                }
                let name = state.slots.get(id).map(|s| s.record.id.clone());
                drop(state);
                let reason = format!("the workspace was not renamed: {e}; {SAVE_HINT}");
                if let Some(name) = name {
                    self.record_op(
                        WorkspaceOpKind::Rename,
                        &name,
                        WorkspaceOpOutcome::Failed,
                        Some(reason.clone()),
                    );
                }
                return Err(WorkspaceError::Unavailable(reason));
            }
            let record = state
                .slots
                .get(id)
                .map(|s| s.record.clone())
                .ok_or_else(|| not_found(id))?;
            drop(state);
            let detail = format!("renamed to {}", record.name);
            self.record_op(
                WorkspaceOpKind::Rename,
                &record.id,
                WorkspaceOpOutcome::Succeeded,
                Some(detail),
            );
            Ok(record)
        })
    }

    fn set_start_folder<'a>(
        &'a self,
        id: &'a WorkspaceId,
        start_folder: Option<StartFolder>,
    ) -> BoxFuture<'a, Result<StartFolderChange, WorkspaceError>> {
        Box::pin(async move {
            let record = {
                let mut state = self.state();
                if state.closed {
                    return Err(WorkspaceError::Unavailable(
                        "Puddle is shutting down".to_owned(),
                    ));
                }
                puddle_api::check_start_folder_change(state.slots.values().map(|s| &s.record), id)?;
                let slot = state.slots.get_mut(id).ok_or_else(|| not_found(id))?;
                let before = std::mem::replace(&mut slot.record.start_folder, start_folder);
                if let Err(e) = self.persist(&state) {
                    // Nothing changes when the list can't be saved.
                    if let Some(slot) = state.slots.get_mut(id) {
                        slot.record.start_folder = before;
                    }
                    return Err(WorkspaceError::Unavailable(format!(
                        "the start folder was not changed: {e}; {SAVE_HINT}"
                    )));
                }
                state
                    .slots
                    .get(id)
                    .map(|s| s.record.clone())
                    .ok_or_else(|| not_found(id))?
            };
            let exists = self.follow_start_folder(&record).await;
            if record.status != WorkspaceStatus::Running {
                // A stopped workspace is looked at when it starts; the old problem ends now.
                for kind in ["start-folder", "start-folder-file"] {
                    self.inner.problems.clear(&problem_key(kind, &record.id));
                }
            }
            Ok(StartFolderChange::new(record, exists))
        })
    }

    fn set_session_user<'a>(
        &'a self,
        id: &'a WorkspaceId,
        user: SessionUser,
    ) -> BoxFuture<'a, Result<WorkspaceRecord, WorkspaceError>> {
        Box::pin(async move {
            let mut state = self.state();
            if state.closed {
                return Err(WorkspaceError::Unavailable(
                    "Puddle is shutting down".to_owned(),
                ));
            }
            puddle_api::check_session_user_change(state.slots.values().map(|s| &s.record), id)?;
            let slot = state.slots.get_mut(id).ok_or_else(|| not_found(id))?;
            let before = std::mem::replace(&mut slot.record.session_user, user);
            if let Err(e) = self.persist(&state) {
                // Nothing changes when the list can't be saved.
                if let Some(slot) = state.slots.get_mut(id) {
                    slot.record.session_user = before;
                }
                return Err(WorkspaceError::Unavailable(format!(
                    "the user the workspace runs as was not changed: {e}; {SAVE_HINT}"
                )));
            }
            state
                .slots
                .get(id)
                .map(|s| s.record.clone())
                .ok_or_else(|| not_found(id))
        })
    }

    fn attach<'a>(
        &'a self,
        id: &'a WorkspaceId,
        mode: AttachMode,
    ) -> BoxFuture<'a, Result<Attached, WorkspaceError>> {
        Box::pin(async move {
            let record = self.get(id).await?;
            if record.busy.is_some() || record.status != WorkspaceStatus::Running {
                return Err(WorkspaceError::Conflict(format!(
                    "{} is not running; start it first",
                    record.name
                )));
            }
            self.refresh_editor_file(&record).await;
            match mode {
                AttachMode::Browser => Ok(self.attach_browser(&record).await),
                AttachMode::Desktop => Ok(match self.desktop_target(&record).await {
                    Err(reason) => Attached::not_opened(reason.to_string()),
                    Ok(target) => match self.inner.launcher.open_desktop(&record, &target).await {
                        Ok(()) => Attached::opened(),
                        Err(err) => Attached::not_opened(err.to_string()),
                    }
                    .with_open_uri(target.open_uri()),
                }),
            }
        })
    }

    fn kept_logins<'a>(
        &'a self,
        id: &'a WorkspaceId,
    ) -> BoxFuture<'a, Result<KeptLogins, WorkspaceError>> {
        Box::pin(async move {
            let record = self.get(id).await?;
            let list = self.inner.injection.kept_logins(&record.id).await;
            Ok(KeptLogins {
                logins: list
                    .logins
                    .into_iter()
                    .map(|login| kept_login_row(&login))
                    .collect(),
                store_readable: list.store_answered,
            })
        })
    }

    fn forget_login<'a>(
        &'a self,
        id: &'a WorkspaceId,
        service: &'a str,
    ) -> BoxFuture<'a, Result<(), WorkspaceError>> {
        Box::pin(async move {
            let record = self.get(id).await?;
            match self.inner.injection.forget_login(&record.id, service).await {
                Ok(true) => Ok(()),
                Ok(false) => Err(WorkspaceError::NotFound(format!(
                    "no login service {service:?}"
                ))),
                Err(error) => Err(WorkspaceError::Unavailable(error.to_string())),
            }
        })
    }

    fn check_shell_init<'a>(
        &'a self,
        id: &'a WorkspaceId,
        script: &'a str,
    ) -> BoxFuture<'a, Result<ShellInitCheck, WorkspaceError>> {
        Box::pin(async move {
            let record = self.get(id).await?;
            let running = self
                .inner
                .live
                .lock()
                .await
                .get(&record.id)
                .and_then(|live| live.gated.clone());
            let Some(gated) = running else {
                return Ok(ShellInitCheck::NotRun(format!(
                    "{} is not running; Puddle checks the script when it starts",
                    record.name
                )));
            };
            match self.run_shell_init_check(&record.id, &gated, script).await {
                Ok(None) => Ok(ShellInitCheck::Passed),
                Ok(Some(why)) => Ok(ShellInitCheck::Refused(why)),
                Err(reason) => Err(WorkspaceError::unavailable(reason)),
            }
        })
    }

    fn orphan_volumes(&self) -> BoxFuture<'_, Result<Vec<WorkspaceId>, WorkspaceError>> {
        Box::pin(async move { self.orphan_volume_ids().await })
    }

    fn restore_orphan<'a>(
        &'a self,
        id: &'a WorkspaceId,
        repository: Option<puddle_api::RepoUrl>,
        git: &'a dyn GitSetup,
    ) -> BoxFuture<'a, Result<WorkspaceRecord, WorkspaceError>> {
        Box::pin(async move { self.restore_orphan_volume(id, repository, git).await })
    }

    fn checkouts<'a>(
        &'a self,
        id: &'a WorkspaceId,
    ) -> BoxFuture<'a, Result<puddle_api::Checkouts, WorkspaceError>> {
        Box::pin(async move { self.checkouts_of(id).await })
    }

    fn add_repository<'a>(
        &'a self,
        id: &'a WorkspaceId,
        add: puddle_api::AddRepository,
        git: &'a dyn GitSetup,
    ) -> BoxFuture<'a, Result<WorkspaceRecord, WorkspaceError>> {
        Box::pin(async move { self.add_repository_to(id, add, git).await })
    }

    fn clone_again<'a>(
        &'a self,
        id: &'a WorkspaceId,
        entry: &'a EntryId,
        git: &'a dyn GitSetup,
    ) -> BoxFuture<'a, Result<WorkspaceRecord, WorkspaceError>> {
        Box::pin(async move { self.clone_entry_again(id, entry, git).await })
    }

    fn remove_repository<'a>(
        &'a self,
        id: &'a WorkspaceId,
        entry: &'a EntryId,
    ) -> BoxFuture<'a, Result<WorkspaceRecord, WorkspaceError>> {
        Box::pin(async move { self.remove_entry(id, entry) })
    }

    fn settings_changed(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move { self.apply_direct_ssh().await })
    }

    fn settings_lock(&self) -> Option<Arc<tokio::sync::Mutex<()>>> {
        Some(self.inner.settings_lock.clone())
    }
}
