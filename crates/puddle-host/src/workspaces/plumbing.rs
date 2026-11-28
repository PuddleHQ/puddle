// SPDX-License-Identifier: AGPL-3.0-or-later
//! The service's construction, state access, operation bookkeeping and task plumbing.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use puddle_agent_proto::host_control::{HostMessage, HostReply};
use puddle_compute::Runtime;
use puddle_ports::Ports;
use puddle_services::{workspaces::Repository, workspaces::WorkspaceRecord};
use puddle_settings::{WorkspaceSettings, host_cpu_count, resolve, resolve_on_host};
use puddle_types::{
    CpuCount, Event, EventSink, MemoryMib, WorkspaceId, WorkspaceStatus, WorkspaceStep,
};

use tokio::sync::watch;
use tokio::task::JoinSet;

use super::{
    EndExtensionHostsError, HostWorkspaces, ImageNote, Inner, Live, Parts, Slot, State, StateGuard,
    SwitchEditorBuildError, Warmth, WriteNames, name_or_id,
};
use crate::HostError;

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

    /// Says which workspace's agent session ended, each time one does.
    pub(crate) fn agent_sessions_ended(&self) -> puddle_ports::EndedSessions {
        self.inner.ports.agent_sessions_ended()
    }

    /// The ports the sandboxes' agents report to (tests drive an agent session's end through it).
    #[must_use]
    pub fn ports(&self) -> &Ports {
        &self.inner.ports
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

    /// Marks the workspace as one whose disk is gone.
    pub(super) fn mark_volume_missing(&self, id: &WorkspaceId) {
        if let Some(slot) = self.state().slots.get_mut(id) {
            slot.volume_missing = true;
        }
    }
}
