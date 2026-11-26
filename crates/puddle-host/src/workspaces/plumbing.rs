// SPDX-License-Identifier: AGPL-3.0-or-later
//! The service's construction, state access, operation bookkeeping and task plumbing.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use puddle_agent_proto::host_control::{HostMessage, HostReply};
use puddle_compute::{ImageConfig, PullPhase, PullProgress, Runtime};
use puddle_ports::Ports;
use puddle_services::workspaces::phase::{Move, Next, Outcome, begin};
use puddle_services::{
    workspaces::Operation, workspaces::Repository, workspaces::WorkspaceError,
    workspaces::WorkspaceRecord,
};
use puddle_settings::{WorkspaceSettings, host_cpu_count, resolve, resolve_on_host};
use puddle_store::{Actor, NewWorkspaceOp, WorkspaceOpKind, WorkspaceOpOutcome};
use puddle_types::{
    CpuCount, DiskKind, Event, EventSink, ImageRef, MemoryMib, Problem, WorkspaceId,
    WorkspaceStatus, WorkspaceStep,
};

use tokio::sync::watch;
use tokio::task::JoinSet;

use super::{
    EndExtensionHostsError, HostWorkspaces, ImageNote, Inner, Leftovers, Live, MAINTENANCE_WAIT,
    Parts, Running, SESSION_END_GAP, SESSION_END_READS, Slot, State, StateGuard,
    SwitchEditorBuildError, Ticket, Warmth, WriteNames, disk_used, name_or_id, not_found,
    wait_until_downloaded,
};
use crate::HostError;

/// Key prefix of the notice that the host drive is full, one per workspace.
const HOST_DISK_FULL: &str = "host-disk-full:";
use crate::files::Stored;
use crate::op_error::OpError;
use crate::settings_read::SettingsReadError;

/// Why a create could not keep what it chose: the workspace's own CPU count in its settings file,
/// then the workspace list. It displays as the reason shown to the user.
#[derive(Debug, thiserror::Error)]
pub(super) enum CreateSaveError {
    /// The workspace's settings could not be read.
    #[error(transparent)]
    Read(#[from] SettingsReadError),
    /// The settings could not be turned into a document.
    #[error(transparent)]
    Document(#[from] puddle_settings::SettingsError),
    /// The settings store refused the document.
    #[error(transparent)]
    Store(#[from] puddle_api::SettingsRepoError),
    /// The workspace list could not be saved.
    #[error(transparent)]
    List(#[from] HostError),
}

impl<R: Runtime + Clone> HostWorkspaces<R> {
    /// The service over `parts`, with the workspaces the book held and the state each sandbox
    /// was left in by reconcile (no sandbox: stopped).
    pub(crate) fn new(
        parts: Parts<R>,
        restored: Vec<Stored>,
        mut retired: BTreeSet<WorkspaceId>,
        mut cleaned: BTreeSet<WorkspaceId>,
        status: &BTreeMap<WorkspaceId, WorkspaceStatus>,
    ) -> Result<Self, HostError> {
        let mut slots = BTreeMap::new();
        let mut interrupted = false;
        for stored in restored {
            if stored.deleting {
                if status.get(&stored.id) == Some(&WorkspaceStatus::VolumeMissing) {
                    // The delete got as far as the volume before Puddle died. The workspace is
                    // gone: its id is retired, and the sweep that follows the start clears
                    // everything else that is keyed by it.
                    tracing::warn!(workspace = %stored.id, "finishing a delete that Puddle did not");
                    retired.insert(stored.id);
                    interrupted = true;
                    continue;
                }
                // The volume is still there, so the delete never reached what holds the user's
                // files: the workspace is listed again and the user can delete it again.
                tracing::warn!(workspace = %stored.id, "a delete did not reach the volume; the workspace stays");
                interrupted = true;
            }
            if stored.creating {
                // A create that never finished (Puddle died in it): there is no workspace, and
                // reconcile removes the half-made volume. Its id is retired like a failed create's.
                tracing::warn!(workspace = %stored.id, "dropping a workspace whose create never finished");
                retired.insert(stored.id);
                interrupted = true;
                continue;
            }
            let (id, name) = (stored.id, stored.name);
            let mut record =
                WorkspaceRecord::new(id.clone(), stored.owner, name.clone(), stored.repositories);
            record.start_folder = stored.start_folder;
            record.session_user = stored.session_user;
            record.image = stored.image;
            record.memory = MemoryMib::new(stored.memory_mib).map_err(|e| HostError::State {
                what: "the workspace list",
                reason: format!(
                    "workspace {name} has an invalid memory size ({e}); fix it in workspaces.json in Puddle's data folder, or remove that entry (its volume is kept)"
                ),
            })?;
            record.created_at = stored.created_at;
            record.disk_size_mib = stored.disk_size_mib;
            record.root_disk_grown_mib = stored.root_disk_grown_mib;
            record.status = match status.get(&id) {
                Some(WorkspaceStatus::Crashed) => WorkspaceStatus::Crashed,
                Some(WorkspaceStatus::VolumeMissing) => WorkspaceStatus::VolumeMissing,
                _ => WorkspaceStatus::Stopped,
            };
            slots.insert(id, Slot::new(record, false));
        }
        // An id retired again above was not cleaned since.
        cleaned.retain(|id| retired.contains(id));
        let service = Self {
            inner: Arc::new(Inner {
                runtime: parts.runtime,
                workspaces: parts.workspaces,
                lifecycle: parts.lifecycle,
                kit: parts.kit,
                events: parts.events,
                clock: parts.clock,
                settings: parts.settings,
                store: parts.store,
                all_users: parts.all_users,
                launcher: parts.launcher,
                ssh: parts.ssh,
                terminals: Arc::default(),
                book: parts.book,
                injection: parts.injection,
                problems: parts.problems,
                reports: parts.reports,
                ports: parts.ports,
                flavours: parts.flavours,
                front_addr: parts.front_addr,
                front_guests: parts.front_guests,
                front_halves: parts.front_halves,
                ids: parts.ids,
                login_markers: parts.login_markers,
                space: parts.space,
                rewrite_gate: parts.rewrite_gate,
                names_pending: Mutex::new(BTreeMap::new()),
                names_writing: Mutex::new(()),
                state: Mutex::new(State {
                    slots,
                    cleaned,
                    retired,
                    closed: false,
                }),
                live: tokio::sync::Mutex::new(BTreeMap::new()),
                tasks: Mutex::new(JoinSet::new()),
                running: watch::channel(0).0,
                maintenance_image: watch::channel(Warmth::Cold).0,
                default_image_note: Mutex::new(ImageNote::Cold),
                shell_init_changed_inside: Mutex::new(BTreeSet::new()),
                env_missed_while_booting: Mutex::new(BTreeSet::new()),
                listings: Mutex::new(BTreeMap::new()),
                disk_warned: Mutex::new(BTreeSet::new()),
                tools: super::tools::ToolRuns::default(),
            }),
        };
        service.note_names(&service.state());
        service.record_owners()?;
        if interrupted && let Err(e) = service.persist(&service.state()) {
            // Harmless: the entries still say "being created" or "being deleted", so the next
            // start settles them the same way, and every later save writes the list without them.
            tracing::warn!(error = %e, "the workspace list was not saved after dropping unfinished creates");
        }
        Ok(service)
    }

    /// Records, on every start, the owner the list names for each listed workspace as that
    /// workspace's user in the database, before anything serves them (recording one again is a
    /// no-op). A list and a database that disagree about whose a workspace is stop the start:
    /// neither is guessed.
    pub(super) fn record_owners(&self) -> Result<(), HostError> {
        for slot in self.state().slots.values() {
            let record = &slot.record;
            self.inner
                .store
                .set_workspace_owner(&record.id, record.owner)
                .map_err(|e| HostError::State {
                    what: "the database",
                    reason: format!(
                        "whose workspace {} is could not be recorded: {e}",
                        record.id
                    ),
                })?;
        }
        Ok(())
    }

    /// What the user calls workspace `id`, for text they read. Never the id: a real host draws
    /// random ones. A workspace the list no longer holds reads as its id, the one thing left.
    pub(super) fn label(&self, id: &WorkspaceId) -> String {
        let name = self
            .state()
            .slots
            .get(id)
            .map(|slot| slot.record.name.clone());
        name_or_id(name.as_ref(), id)
    }

    pub(super) fn state(&self) -> StateGuard<'_, R> {
        StateGuard {
            guard: self
                .inner
                .state
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
            _then: WriteNames(self),
        }
    }

    /// Writes the book. Called with the state lock held, so saves cannot reorder.
    pub(super) fn persist(&self, state: &State) -> Result<(), HostError> {
        let stored = state
            .slots
            .values()
            .map(|slot| Stored {
                id: slot.record.id.clone(),
                owner: slot.record.owner,
                name: slot.record.name.clone(),
                repositories: slot.record.repositories.clone(),
                start_folder: slot.record.start_folder.clone(),
                session_user: slot.record.session_user,
                image: slot.record.image.clone(),
                memory_mib: slot.record.memory.get(),
                created_at: slot.record.created_at,
                disk_size_mib: slot.record.disk_size_mib,
                root_disk_grown_mib: slot.record.root_disk_grown_mib,
                creating: slot.creating,
                deleting: slot.deleting,
            })
            .collect();
        self.inner
            .book
            .save(stored, &state.retired, &state.cleaned)?;
        self.note_names(state);
        Ok(())
    }

    /// Tells the store each workspace's name, so the audit records written from now on carry it
    /// (a record of a renamed or deleted workspace then still reads as the user knew it). Best
    /// effort: a name that cannot be written leaves the id to be shown for new records.
    ///
    /// Only queues the names: the write goes through the store's one connection, which others
    /// wait for, so it must not happen while the state lock is held. [`StateGuard`] writes the
    /// queue once it has released the lock.
    pub(super) fn note_names(&self, state: &State) {
        let mut pending = self
            .inner
            .names_pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        for slot in state.slots.values().filter(|slot| !slot.deleting) {
            pending.insert(slot.record.id.clone(), slot.record.name.to_string());
        }
    }

    /// Writes the queued names to the store. Writers go one at a time and each takes what is
    /// queued when its turn comes, so an older name never lands after a newer one.
    pub(super) fn write_noted_names(&self) {
        let _writing = self
            .inner
            .names_writing
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let batch = std::mem::take(
            &mut *self
                .inner
                .names_pending
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        );
        for (id, name) in batch {
            self.inner.store.note_workspace_name(&id, &name);
        }
    }

    /// The create of `id` finished: saves its entry as complete. Until this succeeds the list
    /// says the create never finished, and the next start would remove its volume.
    pub(super) fn mark_created(&self, id: &WorkspaceId) -> Result<(), OpError> {
        let mut state = self.state();
        // A slot that is gone has nothing to mark; a create holds the workspace busy, so none
        // can go while it runs.
        if let Some(slot) = state.slots.get_mut(id) {
            slot.creating = false;
        }
        let saved = self.persist(&state);
        if saved.is_err()
            && let Some(slot) = state.slots.get_mut(id)
        {
            slot.creating = true;
        }
        saved.map_err(|source| OpError::NewNotSaved { source })
    }

    pub(super) fn status_event(&self, record: &WorkspaceRecord) {
        self.inner.events.emit(Event::StatusChanged {
            workspace: record.id.clone(),
            status: record.status,
        });
    }

    pub(super) fn progress(&self, name: &WorkspaceId, step: WorkspaceStep, detail: Option<String>) {
        self.inner.events.emit(Event::WorkspaceProgress {
            workspace: name.clone(),
            step,
            detail,
            repository: None,
            bytes_done: None,
            bytes_total: None,
        });
    }

    /// A pull's count as the step it belongs to (downloading or unpacking), with the bytes done
    /// and, when known, the bytes in all.
    fn progress_pull(&self, name: &WorkspaceId, pull: PullProgress) {
        let step = match pull.phase {
            PullPhase::Downloading => WorkspaceStep::PullingImage,
            PullPhase::Unpacking => WorkspaceStep::UnpackingImage,
        };
        self.inner.events.emit(Event::WorkspaceProgress {
            workspace: name.clone(),
            step,
            detail: None,
            repository: None,
            bytes_done: Some(pull.done),
            bytes_total: pull.total,
        });
    }

    /// The cloning step of one repository: the address as the detail, the directory as the
    /// repository it is about.
    pub(super) fn progress_cloning(&self, name: &WorkspaceId, repository: &Repository) {
        self.inner.events.emit(Event::WorkspaceProgress {
            workspace: name.clone(),
            step: WorkspaceStep::Cloning,
            detail: Some(repository.url.as_str().to_owned()),
            repository: Some(repository.directory.clone()),
            bytes_done: None,
            bytes_total: None,
        });
    }

    /// Stops taking new operations (they get "shutting down"). Running ones go on.
    pub fn close(&self) {
        self.state().closed = true;
    }

    /// Waits until no operation runs, at most `grace`; then ends every task the service owns,
    /// the background ones no operation counts (a tool install, a checkout refresh, an image
    /// download) as well, so none outlives the host or keeps the store open. Returns how many
    /// operations still ran and had to be ended.
    pub(crate) async fn finish_operations(&self, grace: Duration) -> usize {
        let mut count = self.inner.running.subscribe();
        // Only whether it went idle: the borrow `wait_for` returns must not live across the
        // awaits below, or the shutdown future could not move between threads.
        let idle = tokio::time::timeout(grace, count.wait_for(|n| *n == 0))
            .await
            .is_ok();
        let ended = if idle {
            0
        } else {
            let ended = *self.inner.running.borrow();
            tracing::warn!(ended, "shutdown: ending workspace operations still running");
            ended
        };
        let mut tasks = std::mem::take(&mut *self.tasks());
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
        ended
    }

    /// Closes what the sandboxes own: SSH endpoints and egress routes. The sandboxes are
    /// stopped by the [`Lifecycle`] before this.
    pub(crate) async fn release_all(&self) {
        // The sandboxes are stopped, so no CA is needed any more.
        for name in self.inner.injection.running_workspaces() {
            self.inner.injection.end(&name);
        }
        let live = std::mem::take(&mut *self.inner.live.lock().await);
        for (name, entry) in live {
            let Live { route, ssh, .. } = entry;
            if let Some(ssh) = ssh {
                self.close_ssh(&name, ssh).await;
            }
            route.shutdown().await;
        }
    }

    /// What `workspace` decrypts, and the CA that certifies it, while its sandbox runs: `None`
    /// for a workspace that is stopped or never started.
    #[must_use]
    pub fn termination(&self, workspace: &WorkspaceId) -> Option<Arc<puddle_proxy::Termination>> {
        self.inner.injection.termination(workspace)
    }

    /// How many terminals are open now, in every workspace: the quit asks before it ends them.
    #[must_use]
    pub fn open_terminals(&self) -> usize {
        self.inner.terminals.count()
    }

    /// Ends the editor's extension hosts in `workspace`'s sandbox, and what they started
    /// (language servers, debuggees), because the sandbox's editor window closed. Terminals and the
    /// editor server stay. Returns once the guest agent says it is done.
    ///
    /// # Errors
    ///
    /// [`EndExtensionHostsError::NotRunning`] when the workspace has no connected agent, and
    /// [`EndExtensionHostsError::Agent`] when the agent could not be reached or did not answer.
    pub async fn end_extension_hosts(
        &self,
        workspace: &WorkspaceId,
    ) -> Result<HostReply, EndExtensionHostsError> {
        let link = self
            .inner
            .ports
            .guest_link(workspace)
            .ok_or(EndExtensionHostsError::NotRunning)?;
        link.send(&HostMessage::EndExtensionHosts)
            .await
            .map_err(EndExtensionHostsError::Agent)
    }

    /// Restarts `workspace`'s editor server on a newer build the agent has downloaded and checked,
    /// if there is one, and says whether it did. `connected_windows` is the front's count of
    /// windows connected to that server (the front counts them): a server with a
    /// window on it is never restarted, so the caller asks again at its next quiet moment (the
    /// agent keeps the staged build until then). Only Microsoft's server updates, and only with
    /// the user's automatic-update setting on; for anything else the answer is `false`.
    ///
    /// # Errors
    ///
    /// [`SwitchEditorBuildError`]: a window is connected, the workspace has no connected agent,
    /// or the agent did not answer.
    pub async fn switch_editor_build(
        &self,
        workspace: &WorkspaceId,
        connected_windows: usize,
    ) -> Result<bool, SwitchEditorBuildError> {
        if connected_windows > 0 {
            return Err(SwitchEditorBuildError::WindowsConnected(connected_windows));
        }
        let link = self
            .inner
            .ports
            .guest_link(workspace)
            .ok_or(SwitchEditorBuildError::NotRunning)?;
        let agent_reply = link
            .send(&HostMessage::SwitchEditorBuild)
            .await
            .map_err(SwitchEditorBuildError::Agent)?;
        Ok(agent_reply == HostReply::EditorBuild { switched: true })
    }

    /// Asks each running workspace on Microsoft's server that `windows` counts no window on to
    /// switch to a newer build, and says how many it asked. The host does this every few minutes;
    /// this is one round of it.
    pub async fn switch_idle_editor_builds(&self, windows: &puddle_front::Windows) -> usize {
        crate::editor_updates::ask_once(self, windows).await
    }

    /// The SSH endpoint of a running workspace's sandbox.
    pub async fn ssh_endpoint(&self, workspace: &WorkspaceId) -> Option<std::path::PathBuf> {
        let live = self.inner.live.lock().await;
        let ssh = live.get(workspace)?.ssh.as_ref()?;
        Some(ssh.endpoint().path().to_path_buf())
    }

    /// Begins `op` on `id` when its phase admits it (see [`puddle_services::workspaces::phase`]): the busy mark
    /// goes on and the status becomes the one `op` shows while it runs.
    pub(super) fn accept(
        &self,
        id: &WorkspaceId,
        op: Operation,
    ) -> Result<Ticket<R>, WorkspaceError> {
        let mut state = self.state();
        if state.closed {
            return Err(WorkspaceError::Unavailable(
                "Puddle is shutting down".to_owned(),
            ));
        }
        let slot = state.slots.get_mut(id).ok_or_else(|| not_found(id))?;
        begin(&mut slot.record, op)?;
        Ok(self.ticket(slot.record.clone(), op))
    }

    /// The ticket of an operation that has just marked `record` busy.
    pub(super) fn ticket(&self, record: WorkspaceRecord, op: Operation) -> Ticket<R> {
        Ticket {
            service: self.clone(),
            record,
            op,
            armed: true,
        }
    }

    /// Takes off the busy mark of an operation that never started its work. A create that never
    /// started leaves nothing to keep: its entry goes and its id is retired, like a failed
    /// create's. The status stays: whatever set it since (a crash the monitor saw) still holds.
    pub(super) fn withdraw(&self, id: &WorkspaceId, op: Operation) {
        let mut state = self.state();
        let Some(slot) = state.slots.get_mut(id) else {
            return;
        };
        if slot.record.apply(Move::Withdraw) == Ok(Next::Removed) {
            state.slots.remove(id);
            state.retired.insert(id.clone());
            if let Err(e) = self.persist(&state) {
                // Harmless: the saved entry says "being created", so the next start drops it.
                tracing::warn!(workspace = %id, error = %e, "the workspace list was not saved after a create was dropped");
            }
            return;
        }
        slot.deleting = false;
        slot.delete_note = None;
        let record = slot.record.clone();
        drop(state);
        tracing::info!(workspace = %id, %op, "a request was dropped before its work began");
        // The mark is off: say so, as the end of an operation does.
        self.status_event(&record);
    }

    pub(super) fn tasks(&self) -> MutexGuard<'_, JoinSet<()>> {
        self.inner
            .tasks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Spawns `work` on a task the service owns: it runs to its end whatever becomes of the
    /// request that started it, unless shutdown ends it.
    pub(super) fn spawn(&self, work: impl Future<Output = ()> + Send + 'static) {
        // Shutdown has taken the tasks to end them: work started after it would never be.
        if self.state().closed {
            return;
        }
        let mut tasks = self.tasks();
        while tasks.try_join_next().is_some() {}
        tasks.spawn(work);
    }

    /// Runs `job` on a task the service owns and returns what it returns. A caller dropped while
    /// it waits leaves the job to finish: a maintenance sandbox the job booted is stopped and
    /// removed, and its hold on the volume given back, only once the job has done so. Shutdown
    /// waits for the job like for an operation.
    pub(super) async fn on_own_task<T: Send + 'static>(
        &self,
        job: impl Future<Output = T> + Send + 'static,
    ) -> Result<T, WorkspaceError> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let running = Running::new(&self.inner.running);
        self.spawn(async move {
            let _running = running;
            // Nobody waits any more when the request was dropped: the outcome goes with it.
            let _ = tx.send(job.await);
        });
        rx.await
            .map_err(|_| WorkspaceError::Unavailable("Puddle is shutting down".to_owned()))
    }

    /// Runs `work` on a task the service owns and settles the workspace when it ends. The task
    /// takes over the busy mark of `ticket`; nothing awaits between the two.
    pub(super) fn run<F>(&self, mut ticket: Ticket<R>, work: F)
    where
        F: Future<Output = Result<(), OpError>> + Send + 'static,
    {
        let this = self.clone();
        let (id, op) = (ticket.record.id.clone(), ticket.op);
        let running = Running::new(&self.inner.running);
        self.spawn(async move {
            let _running = running;
            let result = work.await;
            this.refresh_disk_use().await;
            this.conclude(&id, op, result);
        });
        ticket.armed = false;
    }

    /// Says which workspace's agent session ended, each time one does.
    pub(crate) fn agent_sessions_ended(&self) -> puddle_ports::EndedSessions {
        self.inner.ports.agent_sessions_ended()
    }

    /// The ports the sandboxes' agents report to (tests drive an agent session's end through it).
    #[must_use]
    pub fn ports(&self) -> &Ports {
        &self.inner.ports
    }

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

    /// Ends an operation: the busy mark goes, the state follows the outcome, and the last event
    /// of the operation says how it went.
    pub(super) fn conclude(&self, id: &WorkspaceId, op: Operation, result: Result<(), OpError>) {
        let mut state = self.state();
        let Some(slot) = state.slots.get_mut(id) else {
            return;
        };
        slot.deleting = false;
        let delete_note = slot.delete_note.take();
        let leftovers = std::mem::replace(&mut slot.leftovers, Leftovers::Unknown);
        let volume_gone = std::mem::take(&mut slot.volume_missing);
        let name = slot.record.id.clone();
        let before = slot.record.status;
        let outcome = match &result {
            Ok(()) => Outcome::Done,
            Err(_) if volume_gone => Outcome::VolumeGone,
            Err(_) => Outcome::Failed,
        };
        let next = slot.record.apply(Move::End(outcome));
        // A failed create, and a delete either way, leave the list different from what is saved:
        // the failed create's entry goes, the failed delete's entry comes back unmarked, and the
        // finished delete's entry goes with its id retired.
        let save = matches!(
            (&result, op),
            (Err(_), Operation::Creating | Operation::Deleting) | (Ok(()), Operation::Deleting)
        );
        let mut result = result;
        let old_entry = slot.creating;
        let status_changed = slot.record.status != before;
        if op == Operation::Creating && outcome == Outcome::Done {
            slot.creating = false;
        }
        // A stop in progress that failed leaves the VM up: its reports go on.
        if before == WorkspaceStatus::Draining && slot.record.status == WorkspaceStatus::Running {
            self.inner.reports.resume(&name);
        }
        if next == Ok(Next::Removed) {
            // A failed create may leave rows and files keyed by its id: retired like a deleted
            // workspace's, so no later one meets them.
            state.slots.remove(id);
            state.retired.insert(id.clone());
            if op == Operation::Deleting && leftovers == Leftovers::Gone {
                state.cleaned.insert(id.clone());
            }
        }
        let record = state.slots.get(id).map(|s| s.record.clone());
        if save && let Err(e) = self.persist(&state) {
            if old_entry {
                // The entry still says "being created", so the next start drops it and removes
                // what is left of the volume: nothing the user worked on.
                tracing::warn!(workspace = %id, error = %e, "the workspace list was not saved after a failed create");
            } else {
                // The workspace is now missing from the saved list: the next start keeps its
                // volume and reports it, but doesn't list the workspace.
                result = result.map_err(|cause| OpError::NotSaved {
                    cause: Box::new(cause),
                    source: e,
                });
            }
        }
        drop(state);
        if let (true, Some(record)) = (status_changed, &record) {
            self.status_event(record);
        }
        self.audit_operation(&name, op, &result, delete_note);
        self.last_step(&name, op, result);
    }

    /// The event that ends an operation, `done` or `failed`. A create or a start that worked then
    /// sets up the managed tools in the background: the start is done whatever mise does, and a
    /// create's clones are on the volume by now.
    fn last_step(&self, name: &WorkspaceId, op: Operation, result: Result<(), OpError>) {
        match result {
            Ok(()) => {
                self.progress(name, WorkspaceStep::Done, None);
                if matches!(op, Operation::Creating | Operation::Starting) {
                    self.set_up_tools_later(name);
                }
            }
            Err(reason) => {
                tracing::warn!(workspace = %name, %op, %reason, "workspace operation failed");
                self.progress(name, WorkspaceStep::Failed, Some(reason.to_string()));
            }
        }
    }

    /// Writes what an operation came to into the audit log: a create or a delete either way, a
    /// start or a stop only when it failed (a start that worked shows in the workspace's state).
    pub(super) fn audit_operation(
        &self,
        name: &WorkspaceId,
        op: Operation,
        result: &Result<(), OpError>,
        delete_note: Option<&'static str>,
    ) {
        let kind = match op {
            Operation::Creating => WorkspaceOpKind::Create,
            Operation::Deleting => WorkspaceOpKind::Delete,
            Operation::Starting if result.is_err() => WorkspaceOpKind::Start,
            Operation::Stopping if result.is_err() => WorkspaceOpKind::Stop,
            _ => return,
        };
        let (outcome, detail) = match result {
            Ok(()) => (
                WorkspaceOpOutcome::Succeeded,
                delete_note.map(str::to_owned),
            ),
            Err(reason) => (WorkspaceOpOutcome::Failed, Some(reason.to_string())),
        };
        self.record_op(kind, name, outcome, detail);
    }

    pub(super) fn record_op(
        &self,
        op: WorkspaceOpKind,
        name: &WorkspaceId,
        outcome: WorkspaceOpOutcome,
        detail: Option<String>,
    ) {
        let new = NewWorkspaceOp {
            op,
            workspace: Some(name.clone()),
            user: None,
            outcome,
            actor: Actor::Api,
            detail,
        };
        self.inner.store.note_workspace_op(&new);
    }

    /// The default memory for a workspace made without one: the settings' value. An unreadable
    /// settings document is an error, so the workspace is not made with a size the user never
    /// chose.
    pub(super) fn default_memory(&self) -> Result<MemoryMib, SettingsReadError> {
        let global = crate::settings_read::global(self.inner.settings.as_ref())?;
        Ok(resolve(&global, None::<&WorkspaceSettings>).memory.value)
    }

    /// The vCPUs for `name`: its override over the global default over the built-in default,
    /// never above the global maximum. The default for this computer when the settings cannot be read (a workspace is not worth a failed start over a CPU count).
    pub(super) fn cpus(&self, name: &WorkspaceId) -> CpuCount {
        let settings = self.inner.settings.as_ref();
        let read = || {
            let global = crate::settings_read::global(settings)?;
            let own = crate::settings_read::workspace(settings, name)?;
            Ok::<_, SettingsReadError>(
                resolve_on_host(&global, Some(&own), host_cpu_count())
                    .cpus
                    .value,
            )
        };
        read().unwrap_or_else(|reason| {
            tracing::warn!(workspace = %name, %reason, "the workspace boots with the default CPU count: the settings cannot be read");
            CpuCount::default_for_host(host_cpu_count())
        })
    }

    /// Writes `cpus` as the workspace's own setting, keeping anything else its document holds.
    /// Under the settings routes' lock, like every other write of that file.
    pub(super) async fn store_own_cpus(
        &self,
        name: &WorkspaceId,
        cpus: CpuCount,
    ) -> Result<(), CreateSaveError> {
        let repo = self.inner.settings.as_ref();
        let _lock = repo.lock().await;
        let mut own = crate::settings_read::workspace(repo, name)?;
        own.overrides.cpus = Some(cpus);
        let document = own.to_document()?;
        repo.save_workspace(name, document)?;
        Ok(())
    }

    /// The workspace's image and what the runtime says about it, pulled if it is not local yet.
    pub(super) async fn pulled_image(
        &self,
        record: &WorkspaceRecord,
        announce_pull: bool,
    ) -> Result<(ImageRef, ImageConfig), OpError> {
        let image = ImageRef::new(&record.image)?;
        if announce_pull {
            self.progress(
                &record.id,
                WorkspaceStep::PullingImage,
                Some(record.image.clone()),
            );
        }
        let config = if announce_pull {
            let throttle = PullThrottle::default();
            let report = |pull: PullProgress| {
                if throttle.lets_through(pull) {
                    self.progress_pull(&record.id, pull);
                }
            };
            self.inner
                .runtime
                .pull_image_reporting(&image, &report)
                .await?
        } else {
            self.image_config(&image).await?
        };
        Ok((image, config))
    }

    /// The image config of `image`, pulling it if needed.
    pub(super) async fn image_config(&self, image: &ImageRef) -> Result<ImageConfig, OpError> {
        Ok(self.inner.runtime.pull_image(image).await?)
    }

    /// The line the default image's labels give, or `None` while they are not read. The first
    /// call starts a task that downloads the image (the create needs it anyway) and keeps the
    /// answer; a failed download is tried again by the next call. Never waits for the download.
    pub(crate) fn default_image_note(&self) -> Option<String> {
        {
            let mut note = self
                .inner
                .default_image_note
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            match &*note {
                ImageNote::Read(line) => return line.clone(),
                ImageNote::Reading => return None,
                ImageNote::Cold => *note = ImageNote::Reading,
            }
        }
        let this = self.clone();
        self.spawn(async move {
            let next = match ImageRef::new(puddle_services::workspaces::DEFAULT_IMAGE) {
                Ok(image) => match this.inner.runtime.pull_image(&image).await {
                    Ok(config) => ImageNote::Read(puddle_services::workspaces::describe_image(&config.labels)),
                    Err(e) => {
                        tracing::warn!(image = %image, error = %e, "the default image was not downloaded to read its description");
                        ImageNote::Cold
                    }
                },
                Err(_) => ImageNote::Read(None),
            };
            *this
                .inner
                .default_image_note
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = next;
        });
        None
    }

    /// Starts pulling the image the delete check boots on a task the service owns, unless it is
    /// cached or a pull runs. The check can't go ahead without it, and the first pull of an image
    /// that isn't the workspace's own takes minutes, longer than a request may.
    pub(crate) fn warm_maintenance_image(&self) {
        let start = self.inner.maintenance_image.send_if_modified(|w| {
            let idle = matches!(w, Warmth::Cold | Warmth::Failed);
            if idle {
                *w = Warmth::Warming;
            }
            idle
        });
        if !start {
            return;
        }
        let this = self.clone();
        self.spawn(async move {
            let image = this.inner.workspaces.config().maintenance_image.clone();
            let next = match this.inner.runtime.pull_image(&image).await {
                Ok(_) => Warmth::Ready,
                Err(e) => {
                    tracing::warn!(image = %image, error = %e, "the image the delete check boots was not downloaded");
                    Warmth::Failed
                }
            };
            this.inner.maintenance_image.send_replace(next);
        });
    }

    /// [`HostWorkspaces::warm_maintenance_image`] when the service starts with workspaces
    /// listed: one of them may be deleted at once.
    pub(crate) fn warm_maintenance_image_for_listed(&self) {
        let listed = !self.state().slots.is_empty();
        if listed {
            self.warm_maintenance_image();
        }
    }

    /// Waits (up to [`MAINTENANCE_WAIT`]) for the image the delete check boots to be in the
    /// cache, starting the download when nothing has. A request that cannot wait that long gets
    /// an answer that says what is going on; the download goes on without it.
    pub(super) async fn maintenance_ready(&self) -> Result<(), WorkspaceError> {
        self.warm_maintenance_image();
        wait_until_downloaded(
            &mut self.inner.maintenance_image.subscribe(),
            MAINTENANCE_WAIT,
        )
        .await
    }

    /// Runs a job that boots the maintenance image: waits for the image first (see
    /// [`HostWorkspaces::maintenance_ready`]), then runs the job on a task the service owns (see
    /// [`HostWorkspaces::on_own_task`]). Every maintenance job a request waits for passes here.
    pub(super) async fn maintenance_job<T: Send + 'static>(
        &self,
        job: impl Future<Output = T> + Send + 'static,
    ) -> Result<T, WorkspaceError> {
        self.maintenance_ready().await?;
        self.on_own_task(job).await
    }

    /// Marks the workspace as one whose disk is gone.
    pub(super) fn mark_volume_missing(&self, id: &WorkspaceId) {
        if let Some(slot) = self.state().slots.get_mut(id) {
            slot.volume_missing = true;
        }
    }
}

/// Keeps a pull's reports to the ones a person can see change: a new phase, a whole percent more
/// when the total is known, and every 8 MiB when it is not. msb reports per chunk, many times a
/// second.
#[derive(Default)]
pub(super) struct PullThrottle(Mutex<Option<(PullPhase, u64)>>);

impl PullThrottle {
    pub(super) fn lets_through(&self, pull: PullProgress) -> bool {
        const UNKNOWN_TOTAL_STEP: u64 = 8 * 1024 * 1024;
        let at = match pull.total {
            Some(total) if total > 0 => pull.done.min(total).saturating_mul(100) / total,
            _ => pull.done / UNKNOWN_TOTAL_STEP,
        };
        let key = (pull.phase, at);
        let mut last = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let new = *last != Some(key);
        *last = Some(key);
        new
    }
}
