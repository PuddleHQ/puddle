// SPDX-License-Identifier: GPL-3.0-or-later
//! The real [`WorkspaceService`]: workspaces on the sandbox runtime.
//!
//! A call checks the request, marks the workspace busy, returns it, and a task owned by the
//! service does the work, reporting each step as an event (the contract of
//! [`WorkspaceService`]). The work is `puddle-workspace` (volumes, holders, trim, delete check),
//! `puddle-boot` (the boot hook and its gate), the egress route of `puddle-proxy` and the SSH
//! endpoint of `puddle-ssh`; the sandboxes are handed to the [`Lifecycle`] so they are trimmed
//! and stopped when the host exits.
//!
//! A sandbox boots from the spec it was created with, and that spec holds the endpoint of its
//! egress route, which only lives as long as this process. So a stopped workspace starts again in
//! the same sandbox only while this process still serves that route; otherwise (after a restart
//! of the host) its sandbox is rebuilt on the same volume. Everything on the volume survives,
//! the root disk does not (ADR 0006, point 3).
//!
//! The workspace list on disk is what the next start keeps volumes for, so it is saved before
//! anything it must cover exists or goes: a create is refused when its entry can't be written,
//! a create whose final save fails is undone before anyone works in it, and a delete is refused
//! when the entry can't be taken out first (principle 7, "your work is never lost by accident").

use crate::agent_reports::{ReportDesk, entrypoint_problem};
use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use futures_util::future::BoxFuture;
use puddle_agent_proto::editor::{EditorServer, ServerKind};
use puddle_agent_proto::forward::OpenError;
use puddle_agent_proto::host_control::{HostMessage, HostReply};
use puddle_api::{
    AttachMode, Attached, BrowserTarget, DEFAULT_IMAGE, DeleteCheck, DesktopTarget, DisplayName,
    EditorLogin, EditorSignIn, GitSetup, KeptLogin, KeptLogins, LaunchError, Launcher, Listing,
    NewWorkspace, Operation, RepoFindings, Repository, ShellInitCheck, StartFolder,
    StartFolderChange, WorkspaceError, WorkspaceRecord, WorkspaceService,
};
use puddle_boot::{BootError, Gate, GatedSandbox};
use puddle_ca::CaCertificate;
use puddle_compute::{ComputeError, DiskSize, ExecRequest, ImageConfig, Runtime, Sandbox as _};
use puddle_lifecycle::Lifecycle;
use puddle_ports::{NoLink, Ports};
use puddle_proxy::Route;
use puddle_settings::{Effective, ReconnectionGrace, ServerChoice, WorkspaceSettings, resolve};
use puddle_ssh::SshEndpoint;
use puddle_store::Clock;
use puddle_types::{
    CheckoutDir, CpuCount, Event, EventSink, GuestEnv, GuestFile, GuestPath, ImageRef, MemoryMib,
    Problem, Problems, WorkspaceId, WorkspaceName, WorkspaceStatus, WorkspaceStep,
};

use crate::ssh_config::{SshSetup, ssh_host};
use crate::ssh_target::{ReportingTarget, report_no_endpoint};
use puddle_workspace::{Findings, Layout, Workspaces};
use tokio::sync::watch;
use tokio::task::JoinSet;

use crate::HostError;
use crate::boot::{BootKit, GuestInputs};
use crate::files::{Stored, WorkspaceBook};
use crate::front::Flavours;
use crate::git_hosts::{
    Authors, GitProblem, authors, credentials_problem, left_out_problem, never_decrypted,
    old_git_problem,
};
use crate::injection::Injection;
use crate::kubernetes::{kubernetes_key, kubernetes_problem};

#[path = "repositories.rs"]
mod repositories;

impl<R: Runtime + Clone> crate::editor_updates::Builds for HostWorkspaces<R> {
    fn on_microsoft_server(&self) -> Vec<WorkspaceName> {
        self.inner.flavours.on_microsoft()
    }

    async fn switch(
        &self,
        workspace: &WorkspaceName,
        connected_windows: usize,
    ) -> Result<bool, SwitchEditorBuildError> {
        self.switch_editor_build(workspace, connected_windows).await
    }
}

impl<R: Runtime + Clone> crate::changes::Changes for HostWorkspaces<R> {
    fn decrypt_changed(&self, workspace: &WorkspaceName) -> Option<puddle_store::WorkspaceGit> {
        Self::decrypt_changed(self, workspace)
    }

    async fn rewrite_authors(&self, workspace: WorkspaceName) {
        Self::rewrite_authors(self, &workspace).await;
    }

    async fn all_git_changed(&self) {
        Self::all_git_changed(self).await;
    }

    fn all_request_rules_changed(&self) {
        for name in self.inner.injection.running_workspaces() {
            Self::decrypt_changed(self, &name);
        }
    }

    async fn environment_changed(&self, workspace: WorkspaceName) {
        Self::environment_changed(self, &workspace).await;
    }

    async fn all_environments_changed(&self) {
        // Together: a credential store that is slow for one workspace's secrets holds up no other.
        futures_util::future::join_all(
            self.inner
                .injection
                .running_workspaces()
                .iter()
                .map(|name| Self::environment_changed(self, name)),
        )
        .await;
    }

    fn shell_init_changed(&self, workspace: WorkspaceName) -> impl Future<Output = ()> + Send {
        Self::shell_init_changed(self, &workspace);
        std::future::ready(())
    }

    fn all_shell_inits_changed(&self) -> impl Future<Output = ()> + Send {
        let names: Vec<WorkspaceName> = self
            .state()
            .slots
            .values()
            .map(|slot| slot.record.name.clone())
            .collect();
        for name in names {
            Self::shell_init_changed(self, &name);
        }
        std::future::ready(())
    }
}

/// Runs when an author rewrite starts, before it waits for its workspace's rewrite lock, and
/// returns once the rewrite may go on; the value it returns is dropped when the rewrite ends. Tests
/// use it to hold one rewrite back and see the rewrites of a workspace run out of order. A host
/// without one lets every rewrite go on at once.
pub type RewriteGate =
    Arc<dyn Fn(&WorkspaceName) -> BoxFuture<'static, Box<dyn Send>> + Send + Sync>;

/// What the service is built from.
pub(crate) struct Parts<R: Runtime + Clone> {
    pub(crate) runtime: R,
    pub(crate) workspaces: Workspaces,
    pub(crate) lifecycle: Arc<Lifecycle<R>>,
    pub(crate) kit: BootKit,
    pub(crate) events: Arc<dyn EventSink>,
    pub(crate) clock: Arc<dyn Clock>,
    pub(crate) settings: Arc<dyn puddle_api::SettingsRepo>,
    pub(crate) launcher: Arc<dyn Launcher>,
    /// The login key and ssh config of direct SSH.
    pub(crate) ssh: Arc<SshSetup>,
    pub(crate) book: WorkspaceBook,
    pub(crate) injection: Arc<Injection>,
    pub(crate) problems: Arc<Problems>,
    /// The guest agents' reports, which a run of a workspace opens and closes.
    pub(crate) reports: Arc<ReportDesk>,
    pub(crate) ports: Ports,
    pub(crate) rewrite_gate: Option<RewriteGate>,
    /// Which editor server each running guest was started with, for the front.
    pub(crate) flavours: Arc<Flavours>,
    /// Where the browser editor's front listens.
    pub(crate) front_addr: std::net::SocketAddr,
    /// The front's lookup of running guests, which the editor's password is asked through.
    pub(crate) front_guests: Arc<dyn puddle_front::Guests>,
    pub(crate) front_halves: Arc<dyn puddle_front::ServerHalves>,
}

struct Slot {
    record: WorkspaceRecord,
    creating: bool,
    /// Being deleted: left out of the saved list, so a delete that went through can't come
    /// back as a workspace without a volume.
    deleting: bool,
    /// The start that is running was refused because the volume is gone; `conclude` turns it
    /// into the `volume_missing` state instead of `crashed`.
    volume_missing: bool,
}

struct State {
    slots: BTreeMap<WorkspaceId, Slot>,
    closed: bool,
}

/// What the running guest was set up with, kept to set it up again with one thing changed.
#[derive(Clone)]
struct GuestState {
    /// The image's config, so a plan can be rebuilt without pulling.
    image: ImageConfig,
    /// This start's CA certificate.
    ca: CaCertificate,
    /// The commit authors the guest holds now.
    authors: Authors,
    /// The user's variables the guest holds now, a secret as its stand-in: what it started with,
    /// or what the last change to them wrote into its login-shell file.
    env: GuestEnv,
    /// How the browser editor's server runs in it; `None` when the host bundles none. The grace
    /// is the setting as of this start.
    editor: Option<EditorServer>,
    /// The development certificate this start gave the guest, so a re-planned boot keeps it.
    dev_cert: Option<puddle_certs::devcert::GuestDevCert>,
    /// Where login shells land: what the saved start folder was at this start, or when it last
    /// changed.
    start_folder: Option<StartFolder>,
    /// The volume's mount point, `/workspaces/<id>`.
    root: GuestPath,
    /// The multi-root file the editor opens when the workspace has two or more checkouts, as the
    /// last attach wrote it; `None` has the hook take Puddle's lines out of it.
    editor_file: Option<GuestFile>,
}

/// What a sandbox owns while this process serves it.
struct Live<R: Runtime> {
    /// The egress route baked into the sandbox's spec; it outlives stops.
    route: Route,
    gate: Gate,
    /// The booted sandbox, while it runs.
    gated: Option<Arc<GatedSandbox<R::Sandbox>>>,
    ssh: Option<SshEndpoint>,
    /// What the running guest was given at boot; `None` while it does not run.
    guest: Option<GuestState>,
    /// The user's variables baked into the sandbox's own spec when it was made. A sandbox keeps
    /// the environment it was made with (a restart cannot change it), so a start with other
    /// variables makes a new sandbox on the same volume instead of restarting this one.
    spec_env: GuestEnv,
    /// Held while the running guest's author files or login-shell environment file are rewritten,
    /// so two changes never run the boot hook at once.
    rewrite: Arc<tokio::sync::Mutex<()>>,
}

struct Inner<R: Runtime + Clone> {
    runtime: R,
    workspaces: Workspaces,
    lifecycle: Arc<Lifecycle<R>>,
    kit: BootKit,
    events: Arc<dyn EventSink>,
    clock: Arc<dyn Clock>,
    settings: Arc<dyn puddle_api::SettingsRepo>,
    launcher: Arc<dyn Launcher>,
    ssh: Arc<SshSetup>,
    book: WorkspaceBook,
    injection: Arc<Injection>,
    problems: Arc<Problems>,
    reports: Arc<ReportDesk>,
    ports: Ports,
    rewrite_gate: Option<RewriteGate>,
    flavours: Arc<Flavours>,
    front_addr: std::net::SocketAddr,
    front_guests: Arc<dyn puddle_front::Guests>,
    front_halves: Arc<dyn puddle_front::ServerHalves>,
    state: Mutex<State>,
    live: tokio::sync::Mutex<BTreeMap<WorkspaceName, Live<R>>>,
    tasks: tokio::sync::Mutex<JoinSet<()>>,
    running: watch::Sender<usize>,
    /// Workspaces whose start found their shell-init folder changed from inside, until `settle`
    /// has said so (with what the boot hook found, as one notice).
    shell_init_changed_inside: Mutex<BTreeSet<WorkspaceName>>,
    /// Workspaces whose variables changed while their guest was booting: `settle` applies the
    /// change to the guest once it runs.
    env_missed_while_booting: Mutex<BTreeSet<WorkspaceName>>,
    /// The last listing of each workspace's checkouts; see [`repositories::CachedListing`].
    listings: Mutex<BTreeMap<WorkspaceId, repositories::CachedListing>>,
}

/// Why [`HostWorkspaces::end_extension_hosts`] did not reach the guest.
#[derive(Debug, thiserror::Error)]
pub enum EndExtensionHostsError {
    /// The workspace is stopped, or its agent has not connected yet.
    #[error("the workspace is not running")]
    NotRunning,
    /// The agent could not be reached or did not answer.
    #[error("the workspace's agent could not end the extension hosts: {0}")]
    Agent(OpenError),
}

/// Why [`HostWorkspaces::switch_editor_build`] did not run a newer build.
#[derive(Debug, thiserror::Error)]
pub enum SwitchEditorBuildError {
    /// A window is connected to the editor server: restarting it would cut the user off.
    #[error("{0} editor window(s) are connected")]
    WindowsConnected(usize),
    /// The workspace is stopped, or its agent has not connected yet.
    #[error("the workspace is not running")]
    NotRunning,
    /// The agent could not be reached or did not answer.
    #[error("the workspace's agent could not switch the editor server build: {0}")]
    Agent(OpenError),
}

/// The workspaces service. Cheap to clone; clones share state.
pub struct HostWorkspaces<R: Runtime + Clone> {
    inner: Arc<Inner<R>>,
}

impl<R: Runtime + Clone> Clone for HostWorkspaces<R> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<R: Runtime + Clone> std::fmt::Debug for HostWorkspaces<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostWorkspaces")
            .field("workspaces", &self.state().slots.len())
            .finish_non_exhaustive()
    }
}

/// The way out after the workspace list could not be saved; the error already names the file.
const SAVE_HINT: &str =
    "check the free disk space and that Puddle may write that file, then try again";

/// What a failed create or start tells the user about what it could not clean up: the reason it
/// failed first, then what is still there and how it goes away.
fn with_leftovers(reason: String, leftovers: &[String]) -> String {
    if leftovers.is_empty() {
        return reason;
    }
    format!(
        "{reason}; Puddle could not clean up after it: {}. Puddle removes a sandbox no workspace \
         claims at its next start, and keeps a volume it does not know, listing it then",
        leftovers.join("; ")
    )
}

/// The key of a problem about one workspace.
fn problem_key(kind: &str, name: &WorkspaceName) -> String {
    format!("{kind}:{name}")
}

/// The key of the problem about one repository of a workspace that could not be cloned.
fn clone_problem_key(name: &WorkspaceName, directory: &CheckoutDir) -> String {
    format!("{}:{directory}", problem_key("clone", name))
}

fn not_found(id: &WorkspaceId) -> WorkspaceError {
    WorkspaceError::NotFound(format!("no workspace {id}"))
}

fn workspace_error(e: &puddle_workspace::WorkspaceError) -> WorkspaceError {
    match e {
        puddle_workspace::WorkspaceError::InUse { .. } => WorkspaceError::Conflict(e.to_string()),
        puddle_workspace::WorkspaceError::NotFound { .. } => {
            WorkspaceError::NotFound(e.to_string())
        }
        other => WorkspaceError::Internal(other.to_string()),
    }
}

/// The use of the volume of workspace `id` among `volumes`.
fn disk_used(volumes: &[puddle_compute::VolumeInfo], id: &WorkspaceId) -> Option<u64> {
    let name = id.volume_name();
    volumes
        .iter()
        .find(|v| v.name == name.as_str())
        .and_then(|v| v.used)
        .map(|used| u64::from(used.as_mib()))
}

fn listing(l: &puddle_workspace::Listing) -> Listing {
    let mut out = Listing::default();
    out.items.clone_from(&l.items);
    out.more = l.more;
    out
}

fn delete_check(report: &puddle_workspace::DeleteReport) -> DeleteCheck {
    let Findings {
        repos,
        other,
        errors,
    } = &report.findings;
    let check = DeleteCheck::new(
        report.workspace.clone(),
        repos
            .iter()
            .map(|r| {
                let mut out = RepoFindings::default();
                out.dir.clone_from(&r.dir);
                out.uncommitted = listing(&r.uncommitted);
                out.unpushed = listing(&r.unpushed);
                out.stashes = listing(&r.stashes);
                out
            })
            .collect(),
        listing(other),
        errors.clone(),
        report.removes_sandbox.clone(),
    );
    if report.volume_missing {
        check.with_volume_missing()
    } else {
        check
    }
}

/// Holds the count of running operations up for as long as it lives.
struct Running(watch::Sender<usize>);

/// How long a window's sign-in waits for the agent of a workspace that runs but has not said
/// hello yet. The hello normally follows the start within a moment; the agent retries a failed
/// connection every few seconds, so this covers a few tries.
const AGENT_HELLO_WAIT: Duration = Duration::from_secs(15);

/// A window's way to the editor's password: asked of the running guest through the front's own
/// lookup, so the secret is read only when a shell has a window to sign in.
struct GuestLogin {
    guests: Arc<dyn puddle_front::Guests>,
    ports: Ports,
    workspace: WorkspaceName,
}

/// What the user is told when `workspace` has no agent link to sign in through.
fn no_link_message(workspace: &WorkspaceName, why: NoLink) -> String {
    match why {
        NoLink::NotRunning => format!("{workspace} is not running; start it first"),
        NoLink::NotConnected => format!(
            "{workspace} is running, but its editor has not connected to Puddle yet. Puddle keeps \
             trying; open the editor again in a moment"
        ),
    }
}

impl EditorLogin for GuestLogin {
    fn password(&self) -> BoxFuture<'_, Result<String, LaunchError>> {
        Box::pin(async move {
            let editor: Arc<dyn puddle_front::Editor> =
                if let Some(target) = self.guests.target(&self.workspace) {
                    target.editor
                } else {
                    let link = self
                        .ports
                        .guest_link_within(&self.workspace, AGENT_HELLO_WAIT)
                        .await
                        .map_err(|why| LaunchError::new(no_link_message(&self.workspace, why)))?;
                    Arc::new(link)
                };
            editor.password().await.map_err(|err| {
                LaunchError::new(format!(
                    "the editor has no password to sign in with yet: {err}"
                ))
            })
        })
    }
}

impl Running {
    fn new(sender: &watch::Sender<usize>) -> Self {
        sender.send_modify(|n| *n += 1);
        Self(sender.clone())
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.0.send_modify(|n| *n = n.saturating_sub(1));
    }
}

impl<R: Runtime + Clone> HostWorkspaces<R> {
    /// The service over `parts`, with the workspaces the book held and the state each sandbox
    /// was left in by reconcile (no sandbox: stopped).
    pub(crate) fn new(
        parts: Parts<R>,
        restored: Vec<Stored>,
        status: &BTreeMap<WorkspaceName, WorkspaceStatus>,
    ) -> Result<Self, HostError> {
        let mut slots = BTreeMap::new();
        let mut interrupted = false;
        for stored in restored {
            if stored.creating {
                // A create that never finished (Puddle died in it): there is no workspace, and
                // reconcile removes the half-made volume.
                tracing::warn!(workspace = %stored.id, "dropping a workspace whose create never finished");
                interrupted = true;
                continue;
            }
            let (id, name) = (stored.id, stored.name);
            let mut record = WorkspaceRecord::new(id.clone(), name.clone(), stored.repositories);
            record.display_name = stored.display_name;
            record.start_folder = stored.start_folder;
            record.image = stored.image;
            record.memory = MemoryMib::new(stored.memory_mib).map_err(|e| HostError::State {
                what: "the workspace list",
                reason: format!(
                    "workspace {name} has an invalid memory size ({e}); fix it in workspaces.json in Puddle's data folder, or remove that entry (its volume is kept)"
                ),
            })?;
            record.created_at = stored.created_at;
            record.disk_size_mib = stored.disk_size_mib;
            record.status = match status.get(&name) {
                Some(WorkspaceStatus::Crashed) => WorkspaceStatus::Crashed,
                Some(WorkspaceStatus::VolumeMissing) => WorkspaceStatus::VolumeMissing,
                _ => WorkspaceStatus::Stopped,
            };
            slots.insert(
                id,
                Slot {
                    record,
                    creating: false,
                    deleting: false,
                    volume_missing: false,
                },
            );
        }
        let service = Self {
            inner: Arc::new(Inner {
                runtime: parts.runtime,
                workspaces: parts.workspaces,
                lifecycle: parts.lifecycle,
                kit: parts.kit,
                events: parts.events,
                clock: parts.clock,
                settings: parts.settings,
                launcher: parts.launcher,
                ssh: parts.ssh,
                book: parts.book,
                injection: parts.injection,
                problems: parts.problems,
                reports: parts.reports,
                ports: parts.ports,
                flavours: parts.flavours,
                front_addr: parts.front_addr,
                front_guests: parts.front_guests,
                front_halves: parts.front_halves,
                rewrite_gate: parts.rewrite_gate,
                state: Mutex::new(State {
                    slots,
                    closed: false,
                }),
                live: tokio::sync::Mutex::new(BTreeMap::new()),
                tasks: tokio::sync::Mutex::new(JoinSet::new()),
                running: watch::channel(0).0,
                shell_init_changed_inside: Mutex::new(BTreeSet::new()),
                env_missed_while_booting: Mutex::new(BTreeSet::new()),
                listings: Mutex::new(BTreeMap::new()),
            }),
        };
        if interrupted && let Err(e) = service.persist(&service.state()) {
            // Harmless: the entries still say "being created", so the next start drops them
            // again, and every later save writes the list without them.
            tracing::warn!(error = %e, "the workspace list was not saved after dropping unfinished creates");
        }
        Ok(service)
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Writes the book. Called with the state lock held, so saves cannot reorder.
    fn persist(&self, state: &State) -> Result<(), HostError> {
        let stored = state
            .slots
            .values()
            .filter(|slot| !slot.deleting)
            .map(|slot| Stored {
                id: slot.record.id.clone(),
                name: slot.record.name.clone(),
                display_name: slot.record.display_name.clone(),
                repositories: slot.record.repositories.clone(),
                start_folder: slot.record.start_folder.clone(),
                image: slot.record.image.clone(),
                memory_mib: slot.record.memory.get(),
                created_at: slot.record.created_at,
                disk_size_mib: slot.record.disk_size_mib,
                creating: slot.creating,
            })
            .collect();
        self.inner.book.save(stored)
    }

    /// The create of `id` finished: saves its entry as complete. Until this succeeds the list
    /// says the create never finished, and the next start would remove its volume.
    fn mark_created(&self, id: &WorkspaceId) -> Result<(), String> {
        let mut state = self.state();
        let Some(slot) = state.slots.get_mut(id) else {
            return Err(not_found(id).to_string());
        };
        slot.creating = false;
        let saved = self.persist(&state);
        if saved.is_err()
            && let Some(slot) = state.slots.get_mut(id)
        {
            slot.creating = true;
        }
        saved.map_err(|e| e.to_string())
    }

    fn status_event(&self, record: &WorkspaceRecord) {
        self.inner.events.emit(Event::StatusChanged {
            workspace: record.name.clone(),
            status: record.status,
        });
    }

    fn progress(&self, name: &WorkspaceName, step: WorkspaceStep, detail: Option<String>) {
        self.inner.events.emit(Event::WorkspaceProgress {
            workspace: name.clone(),
            step,
            detail,
            repository: None,
        });
    }

    /// The cloning step of one repository: the address as the detail, the directory as the
    /// repository it is about.
    fn progress_cloning(&self, name: &WorkspaceName, repository: &Repository) {
        self.inner.events.emit(Event::WorkspaceProgress {
            workspace: name.clone(),
            step: WorkspaceStep::Cloning,
            detail: Some(repository.url.as_str().to_owned()),
            repository: Some(repository.directory.clone()),
        });
    }

    /// Stops taking new operations (they get "shutting down"). Running ones go on.
    pub fn close(&self) {
        self.state().closed = true;
    }

    /// Waits until no operation runs, at most `grace`; then ends the ones still running.
    /// Returns how many had to be ended.
    pub(crate) async fn finish_operations(&self, grace: Duration) -> usize {
        let mut count = self.inner.running.subscribe();
        // Only whether it went idle: the borrow `wait_for` returns must not live across the
        // awaits below, or the shutdown future could not move between threads.
        let idle = tokio::time::timeout(grace, count.wait_for(|n| *n == 0))
            .await
            .is_ok();
        if idle {
            return 0;
        }
        let ended = *self.inner.running.borrow();
        tracing::warn!(ended, "shutdown: ending workspace operations still running");
        let mut tasks = self.inner.tasks.lock().await;
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
    pub fn termination(&self, workspace: &WorkspaceName) -> Option<Arc<puddle_proxy::Termination>> {
        self.inner.injection.termination(workspace)
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
        workspace: &WorkspaceName,
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
        workspace: &WorkspaceName,
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
    pub async fn ssh_endpoint(&self, workspace: &WorkspaceName) -> Option<std::path::PathBuf> {
        let live = self.inner.live.lock().await;
        let ssh = live.get(workspace)?.ssh.as_ref()?;
        Some(ssh.endpoint().path().to_path_buf())
    }

    fn accept(
        &self,
        id: &WorkspaceId,
        op: Operation,
        check: impl FnOnce(&WorkspaceRecord) -> Result<(), WorkspaceError>,
        enter: impl FnOnce(&mut WorkspaceRecord),
    ) -> Result<WorkspaceRecord, WorkspaceError> {
        let mut state = self.state();
        if state.closed {
            return Err(WorkspaceError::Unavailable(
                "Puddle is shutting down".to_owned(),
            ));
        }
        let slot = state.slots.get_mut(id).ok_or_else(|| not_found(id))?;
        if let Some(busy) = slot.record.busy {
            return Err(WorkspaceError::Conflict(format!(
                "{} is busy ({busy}); wait for it to finish",
                slot.record.name
            )));
        }
        check(&slot.record)?;
        slot.record.busy = Some(op);
        enter(&mut slot.record);
        Ok(slot.record.clone())
    }

    /// Runs `work` on a task the service owns and settles the workspace when it ends.
    async fn run<F>(&self, record: &WorkspaceRecord, op: Operation, work: F)
    where
        F: Future<Output = Result<(), String>> + Send + 'static,
    {
        let this = self.clone();
        let id = record.id.clone();
        let running = Running::new(&self.inner.running);
        let mut tasks = self.inner.tasks.lock().await;
        while tasks.try_join_next().is_some() {}
        tasks.spawn(async move {
            let _running = running;
            let result = work.await;
            this.refresh_disk_use().await;
            this.conclude(&id, op, result);
        });
    }

    /// Reads what every workspace's volume takes on the host into the records, so the list and
    /// the single-workspace answer both carry it. A runtime that cannot say leaves the earlier
    /// value; the card shows the size alone until it can.
    async fn refresh_disk_use(&self) {
        let volumes = match self.inner.runtime.list_volumes().await {
            Ok(volumes) => volumes,
            Err(e) => {
                tracing::debug!(error = %e, "the volumes' use of the disk is unknown");
                return;
            }
        };
        for slot in self.state().slots.values_mut() {
            slot.record.disk_used_mib = disk_used(&volumes, &slot.record.id);
        }
    }

    /// Ends an operation: the busy mark goes, the state follows the outcome, and the last event
    /// of the operation says how it went.
    fn conclude(&self, id: &WorkspaceId, op: Operation, result: Result<(), String>) {
        let mut state = self.state();
        let Some(slot) = state.slots.get_mut(id) else {
            return;
        };
        slot.record.busy = None;
        slot.deleting = false;
        let name = slot.record.name.clone();
        let mut status_changed = false;
        // Only a failed create or delete leaves the list different from what is saved: the
        // failed create's entry goes, the failed delete's entry comes back.
        let save = matches!(
            (&result, op),
            (Err(_), Operation::Creating | Operation::Deleting)
        );
        let mut result = result;
        let old_entry = slot.creating;
        match (&result, op) {
            (Err(_), Operation::Creating) | (Ok(()), Operation::Deleting) => {
                state.slots.remove(id);
            }
            (Err(_), Operation::Starting) => {
                slot.record.status = if std::mem::take(&mut slot.volume_missing) {
                    WorkspaceStatus::VolumeMissing
                } else {
                    WorkspaceStatus::Crashed
                };
                status_changed = true;
            }
            (Err(_), Operation::Stopping) => {
                slot.record.status = WorkspaceStatus::Running;
                status_changed = true;
                self.inner.reports.resume(&name);
            }
            (Err(_), Operation::Reclaiming | Operation::Deleting)
            | (Ok(()), Operation::Reclaiming)
            | (_, Operation::Cloning) => {}
            (Ok(()), Operation::Creating | Operation::Starting) => {
                slot.record.status = WorkspaceStatus::Running;
                slot.creating = false;
                status_changed = true;
            }
            (Ok(()), Operation::Stopping) => {
                slot.record.status = WorkspaceStatus::Stopped;
                status_changed = true;
            }
        }
        let record = state.slots.get(id).map(|s| s.record.clone());
        if save && let Err(e) = self.persist(&state) {
            if old_entry {
                // The entry still says "being created", so the next start drops it and removes
                // what is left of the volume: nothing the user worked on.
                tracing::warn!(workspace = %id, error = %e, "the workspace list was not saved after a failed create");
            } else if let Err(reason) = &mut result {
                // The workspace is now missing from the saved list: the next start keeps its
                // volume and reports it, but doesn't list the workspace.
                *reason = format!("{reason}; {e}; {SAVE_HINT}");
            }
        }
        drop(state);
        if let (true, Some(record)) = (status_changed, &record) {
            self.status_event(record);
        }
        match result {
            Ok(()) => self.progress(&name, WorkspaceStep::Done, None),
            Err(reason) => {
                tracing::warn!(workspace = %id, %op, %reason, "workspace operation failed");
                self.progress(&name, WorkspaceStep::Failed, Some(reason));
            }
        }
    }

    /// The default memory for a workspace made without one: the settings' value. An unreadable
    /// settings document is an error, so the workspace is not made with a size the user never
    /// chose.
    fn default_memory(&self) -> Result<MemoryMib, String> {
        let global = crate::settings_read::global(self.inner.settings.as_ref())?;
        Ok(resolve(&global, None::<&WorkspaceSettings>).memory.value)
    }

    /// The vCPUs for `name`: its override over the global default over one. The default when the
    /// settings cannot be read (a workspace is not worth a failed start over a CPU count).
    fn cpus(&self, name: &WorkspaceName) -> CpuCount {
        let settings = self.inner.settings.as_ref();
        let read = || {
            let global = crate::settings_read::global(settings)?;
            let own = crate::settings_read::workspace(settings, name)?;
            Ok::<_, String>(resolve(&global, Some(&own)).cpus.value)
        };
        read().unwrap_or_else(|reason| {
            tracing::warn!(workspace = %name, %reason, "the workspace boots with the default CPU count: the settings cannot be read");
            CpuCount::DEFAULT
        })
    }

    /// The image config of `image`, pulling it if needed.
    async fn image_config(&self, image: &ImageRef) -> Result<ImageConfig, String> {
        self.inner
            .runtime
            .pull_image(image)
            .await
            .map_err(|e| e.to_string())
    }

    /// Creates a sandbox on workspace `record`'s volume and boots it through the hook.
    fn mark_volume_missing(&self, id: &WorkspaceId) {
        if let Some(slot) = self.state().slots.get_mut(id) {
            slot.volume_missing = true;
        }
    }

    async fn boot_new(
        &self,
        record: &WorkspaceRecord,
        created_volume: &mut bool,
        announce_pull: bool,
        fresh: bool,
    ) -> Result<(), String> {
        let inner = &self.inner;
        let (id, name) = (&record.id, &record.name);
        let size = DiskSize::mib(u32::try_from(record.disk_size_mib).unwrap_or(u32::MAX));
        if announce_pull {
            self.progress(name, WorkspaceStep::PreparingVolume, None);
        }
        // A start must find the volume it left; only a create makes one.
        let attachment = if fresh {
            inner
                .workspaces
                .prepare(&inner.runtime, id, &name.sandbox_name(), Some(size))
                .await
        } else {
            inner
                .workspaces
                .prepare_existing(&inner.runtime, id, &name.sandbox_name())
                .await
        }
        .map_err(|e| {
            if matches!(e, puddle_workspace::WorkspaceError::VolumeMissing { .. }) {
                self.mark_volume_missing(id);
            }
            e.to_string()
        })?;
        *created_volume = attachment.created_volume();
        let image = ImageRef::new(&record.image).map_err(|e| e.to_string());
        let config = match &image {
            Ok(image) => {
                if announce_pull {
                    self.progress(
                        name,
                        WorkspaceStep::PullingImage,
                        Some(record.image.clone()),
                    );
                }
                self.image_config(image).await
            }
            Err(e) => Err(e.clone()),
        };
        let config = match config {
            Ok(config) => config,
            Err(reason) => return Err(self.abort_attachment(attachment, reason).await),
        };
        let image = image.map_err(|e| e.clone())?;
        self.progress(name, WorkspaceStep::Starting, None);
        if let Err(reason) = self.publish_shell_init(id, name) {
            return Err(self.abort_attachment(attachment, reason).await);
        }
        let route = match inner.kit.route(name) {
            Ok(route) => route,
            Err(reason) => return Err(self.abort_attachment(attachment, reason).await),
        };
        // The CA comes after the route: a start that fails here has nothing else to take back.
        let (plan, env, guest) = match self.plan_guest_for(record, config).await {
            Ok(ready) => ready,
            Err(reason) => {
                route.shutdown().await;
                return Err(self.abort_attachment(attachment, reason).await);
            }
        };
        let spec = attachment
            .add_to(
                inner
                    .kit
                    .spec(name, image, record.memory, &env, &route)
                    .with_cpus(self.cpus(name).get()),
            )
            .with_file_mount(inner.kit.shell_init().mount(id));
        let gate = Gate::new();
        match inner
            .kit
            .hook()
            .create(&inner.runtime, spec, &plan, &gate)
            .await
        {
            Ok(gated) => {
                attachment.commit();
                inner.flavours.running(name, guest.editor.as_ref());
                inner.live.lock().await.insert(
                    name.clone(),
                    Live {
                        route,
                        gate,
                        gated: Some(Arc::new(gated)),
                        ssh: None,
                        spec_env: guest.env.clone(),
                        guest: Some(guest),
                        rewrite: Arc::default(),
                    },
                );
                Ok(())
            }
            Err(e) => {
                inner.injection.end(name);
                let reason = self.abort_attachment(attachment, boot_message(&e)).await;
                route.shutdown().await;
                Err(reason)
            }
        }
    }

    /// [`Self::plan_guest`] for `record`'s workspace and saved start folder.
    async fn plan_guest_for(
        &self,
        record: &WorkspaceRecord,
        image: ImageConfig,
    ) -> Result<(puddle_boot::BootPlan, GuestEnv, GuestState), String> {
        self.plan_guest(
            &record.id,
            &record.name,
            image,
            record.start_folder.as_ref(),
        )
        .await
    }

    /// Makes this start's CA for `name`, registers what it decrypts and its secrets' stand-ins and
    /// plans the guest's boot with the CA in its trust, the authors of the workspace's identities
    /// and its environment. A start that fails after this must call `Injection::end`.
    async fn plan_guest(
        &self,
        id: &WorkspaceId,
        name: &WorkspaceName,
        image: ImageConfig,
        start_folder: Option<&StartFolder>,
    ) -> Result<(puddle_boot::BootPlan, GuestEnv, GuestState), String> {
        let inner = &self.inner;
        let capture = crate::logins::capture_enabled(inner.settings.as_ref(), name);
        let editor = inner
            .kit
            .editor(id, self.reconnection_grace(name), self.editor_kind())?;
        let began = inner.injection.begin(name, capture).await?;
        let wanted = authors(&began.git);
        self.report_git_settings(name, &began.git, &wanted);
        let state = GuestState {
            editor,
            image,
            ca: began.certificate,
            authors: wanted,
            env: began.env,
            dev_cert: inner.kit.dev_cert().await,
            start_folder: start_folder.cloned(),
            root: Layout::new(id).map_err(|e| e.to_string())?.mount().clone(),
            editor_file: None,
        };
        match inner.kit.plan(
            &state.image,
            &GuestInputs {
                ca: &state.ca,
                authors: &state.authors,
                env: &state.env,
                editor: state.editor.as_ref(),
                dev_cert: state.dev_cert.as_ref(),
                root: &state.root,
                start_folder: state.start_folder.as_ref(),
                editor_file: state.editor_file.as_ref(),
            },
        ) {
            Ok((plan, env)) => Ok((plan, env, state)),
            Err(reason) => {
                inner.injection.end(name);
                Err(reason)
            }
        }
    }

    /// A change to `name`'s Git settings (an identity attached, detached or edited, a coverage
    /// changed): what the running workspace decrypts follows at once, and its guest gets the
    /// authors of the new identities. A workspace that does not run reads the settings at its
    /// next start.
    pub(crate) async fn git_changed(&self, name: &WorkspaceName) {
        if self.decrypt_changed(name).is_some() {
            self.rewrite_authors(name).await;
        }
    }

    /// Recomputes what the running `name` decrypts from the database: the next connection sees
    /// the new set. The settings of a workspace that does not run, or that cannot be read, change
    /// nothing here.
    pub(crate) fn decrypt_changed(
        &self,
        name: &WorkspaceName,
    ) -> Option<puddle_store::WorkspaceGit> {
        let problems = &self.inner.problems;
        match self.inner.injection.resync(name) {
            Ok(git) => {
                GitProblem::DecryptSync.settle(problems, name, None);
                if let Some(git) = &git {
                    self.report_credentials(name, git);
                }
                git
            }
            Err(reason) => {
                GitProblem::DecryptSync.settle(
                    problems,
                    name,
                    Some((
                        format!("Puddle could not update what {name} decrypts"),
                        format!(
                            "{reason}. {name} keeps the credentials it had; the next change to its \
                             identities tries again."
                        ),
                    )),
                );
                None
            }
        }
    }

    /// Tells the user about the credentials of `git` that Puddle never uses.
    fn report_credentials(&self, name: &WorkspaceName, git: &puddle_store::WorkspaceGit) {
        let hosts = never_decrypted(git);
        GitProblem::Credentials.settle(
            &self.inner.problems,
            name,
            credentials_problem(name, &hosts),
        );
    }

    /// Tells the user about what the Git settings of `name` ask for that Puddle cannot carry out.
    fn report_git_settings(
        &self,
        name: &WorkspaceName,
        git: &puddle_store::WorkspaceGit,
        wanted: &Authors,
    ) {
        self.report_credentials(name, git);
        GitProblem::LeftOut.settle(&self.inner.problems, name, left_out_problem(name, wanted));
    }

    /// After a boot: Kubernetes tools in the guest are a problem on the workspace, since its
    /// kernel cannot route their Services. A stop ends it, so a start that finds none has none.
    fn settle_kubernetes(&self, name: &WorkspaceName, report: &puddle_boot::BootReport) {
        if let Some(problem) = kubernetes_problem(name, &report.kubernetes_tools()) {
            self.inner.problems.raise(problem);
        }
    }

    /// After the hook ran with `wanted` authors: a guest whose git is too old for the author rules
    /// is a problem on the workspace.
    fn settle_git_version(
        &self,
        name: &WorkspaceName,
        report: &puddle_boot::BootReport,
        wanted: &Authors,
    ) {
        GitProblem::OldGit.settle(
            &self.inner.problems,
            name,
            old_git_problem(name, report.git_version(), wanted),
        );
    }

    /// Runs the boot plan again in the running guest when its authors differ from what the guest
    /// holds (the hook is idempotent: only the author files change).
    ///
    /// The settings are read once the guest is ours alone: rewrites for one workspace start in
    /// any order, and a copy taken when the change arrived could be applied after a newer one,
    /// leaving the old authors and a problem the newer settings already ended.
    pub(crate) async fn rewrite_authors(&self, name: &WorkspaceName) {
        let inner = &self.inner;
        let _held = match &inner.rewrite_gate {
            Some(gate) => Some(gate(name).await),
            None => None,
        };
        let rewrite = {
            let live = inner.live.lock().await;
            match live.get(name) {
                Some(entry) => Arc::clone(&entry.rewrite),
                None => return, // not registered yet
            }
        };
        let _one_at_a_time = rewrite.lock().await;
        let (gated, guest) = {
            let live = inner.live.lock().await;
            match live
                .get(name)
                .and_then(|e| Some((e.gated.clone()?, e.guest.clone()?)))
            {
                Some(running) => running,
                // Not booted yet (the start reads the settings itself, and `settle` looks again)
                // or stopped.
                None => return, // not up yet
            }
        };
        // The caller's own read of the settings worked, so this one failing is news: say so.
        let git = match inner.injection.git(name) {
            Ok(git) => git,
            Err(reason) => {
                Self::raise_author_sync(&inner.problems, name, &reason);
                return;
            }
        };
        let wanted = authors(&git);
        self.report_git_settings(name, &git, &wanted);
        if guest.authors == wanted {
            return;
        }
        let replanned = GuestState {
            authors: wanted.clone(),
            ..guest
        };
        match self.run_plan_report(name, &gated, &replanned).await {
            Ok(report) => {
                GitProblem::AuthorSync.settle(&inner.problems, name, None);
                self.settle_git_version(name, &report, &wanted);
                if let Some(entry) = inner.live.lock().await.get_mut(name)
                    && let Some(state) = entry.guest.as_mut()
                {
                    state.authors = wanted;
                }
            }
            Err(reason) => Self::raise_author_sync(&inner.problems, name, &reason),
        }
    }

    /// Tells the user the commit authors in `name` could not be updated and why.
    fn raise_author_sync(problems: &Problems, name: &WorkspaceName, reason: &str) {
        GitProblem::AuthorSync.settle(
            problems,
            name,
            Some((
                format!("Puddle could not update the commit authors in {name}"),
                format!(
                    "{reason}. The workspace keeps the authors it had; the next change to its \
                     identities tries again."
                ),
            )),
        );
    }

    /// Looks in the guest for the start folder. A missing one never fails anything:
    /// terminals and the editor open the volume root, and a problem on the workspace names the
    /// folder and the ways out until it exists or the setting changes. `None` for the volume
    /// root, or when the guest could not be asked (the problem stays as it was).
    async fn check_start_folder(
        &self,
        name: &WorkspaceName,
        gated: &GatedSandbox<R::Sandbox>,
        root: &GuestPath,
        start: Option<&StartFolder>,
    ) -> Option<bool> {
        let key = problem_key("start-folder", name);
        let Some(folder) = start else {
            self.inner.problems.clear(&key);
            return None;
        };
        let path = crate::editor_file::start_path(root, start);
        let request = ExecRequest::new(
            "sh",
            ["-c", "[ -d \"$1\" ]", "puddle-start-folder", path.as_str()].map(str::to_owned),
        )
        .as_user("root")
        .with_timeout(Duration::from_secs(10));
        let exists = match gated.ungated().exec(request).await {
            Ok(output) => output.status.success(),
            Err(e) => {
                tracing::warn!(workspace = %name, error = %e, "the start folder was not looked for");
                self.inner.problems.raise(
                    Problem::new(
                        key,
                        format!("Puddle could not look for {name}'s start folder {folder}"),
                        format!(
                            "{e}. New terminals and the editor may not open it; Puddle looks \
                             again at every start and attach."
                        ),
                    )
                    .about(name.clone()),
                );
                return None;
            }
        };
        if exists {
            self.inner.problems.clear(&key);
        } else {
            self.inner.problems.raise(
                Problem::new(
                    key,
                    format!("{name}'s start folder {folder} does not exist"),
                    format!(
                        "New terminals and the editor open the volume root instead. Create the \
                         folder {folder} in the workspace, or change the start folder in the \
                         workspace's settings; Puddle looks again at every start and attach."
                    ),
                )
                .about(name.clone()),
            );
        }
        Some(exists)
    }

    /// The start folder of the running `record` changed: the login-shell file is written again
    /// (the hook is idempotent) and the folder is looked for. `None` when the workspace does not
    /// run, or the folder cannot be looked for.
    async fn follow_start_folder(&self, record: &WorkspaceRecord) -> Option<bool> {
        let name = &record.name;
        let inner = &self.inner;
        let rewrite = inner
            .live
            .lock()
            .await
            .get(name)
            .map(|e| Arc::clone(&e.rewrite))?;
        let _one_at_a_time = rewrite.lock().await;
        let (gated, guest) = {
            let live = inner.live.lock().await;
            live.get(name)
                .and_then(|e| Some((e.gated.clone()?, e.guest.clone()?)))?
        };
        let replanned = GuestState {
            start_folder: record.start_folder.clone(),
            // Opening a folder replaces the multi-root window; the next attach writes it again.
            editor_file: record
                .start_folder
                .as_ref()
                .and(None)
                .or(guest.editor_file.clone()),
            ..guest
        };
        let key = problem_key("start-folder-file", name);
        match self.run_plan(name, &gated, &replanned).await {
            Ok(()) => {
                inner.problems.clear(&key);
                if let Some(state) = inner
                    .live
                    .lock()
                    .await
                    .get_mut(name)
                    .and_then(|e| e.guest.as_mut())
                {
                    state.start_folder.clone_from(&replanned.start_folder);
                    state.editor_file.clone_from(&replanned.editor_file);
                }
            }
            Err(reason) => {
                tracing::warn!(workspace = %name, %reason, "the start folder is not updated in the guest");
                inner.problems.raise(
                    Problem::new(
                        key,
                        format!("Puddle could not move {name}'s terminals to the new start folder"),
                        format!(
                            "{reason}. The setting is saved; new terminals start where they did \
                             until the workspace starts again."
                        ),
                    )
                    .about(name.clone()),
                );
            }
        }
        self.check_start_folder(name, &gated, &replanned.root, record.start_folder.as_ref())
            .await
    }

    /// At an attach: the editor's target. With no start folder and two or more checkouts the
    /// multi-root file is written again (through the boot hook, which merges only `folders` and
    /// leaves a file it cannot parse as it is, with a problem naming it); with fewer, Puddle's
    /// lines are taken out of it.
    async fn refresh_editor_file(&self, record: &WorkspaceRecord) {
        let name = &record.name;
        let inner = &self.inner;
        let Some(rewrite) = inner
            .live
            .lock()
            .await
            .get(name)
            .map(|e| Arc::clone(&e.rewrite))
        else {
            return;
        };
        let _one_at_a_time = rewrite.lock().await;
        let Some((gated, guest)) = ({
            let live = inner.live.lock().await;
            live.get(name)
                .and_then(|e| Some((e.gated.clone()?, e.guest.clone()?)))
        }) else {
            return;
        };
        self.check_start_folder(name, &gated, &guest.root, record.start_folder.as_ref())
            .await;
        let key = problem_key("editor-file", name);
        let not_updated = |reason: &str| {
            tracing::warn!(workspace = %name, %reason, "the editor's target is not updated");
            inner.problems.raise(
                Problem::new(
                    key.clone(),
                    format!("Puddle could not update the editor's file in {name}"),
                    format!(
                        "{reason}. The editor opens what it opened before until the next attach \
                         works."
                    ),
                )
                .about(name.clone()),
            );
        };
        let found = match inner
            .workspaces
            .list_checkouts(gated.ungated(), &record.id)
            .await
        {
            Ok(list) => crate::editor_file::Found::from_listing(&list),
            Err(e) => {
                not_updated(&e.to_string());
                return;
            }
        };
        let order: Vec<CheckoutDir> = record
            .repositories
            .iter()
            .map(|r| r.directory.clone())
            .collect();
        let file = crate::editor_file::multi_root_folders(
            &guest.root,
            record.start_folder.as_ref(),
            &found,
            &order,
        )
        .and_then(|folders| crate::editor_file::workspace_file(&guest.root, &record.id, &folders));
        let replanned = GuestState {
            editor_file: file,
            ..guest.clone()
        };
        let report = match self.run_plan_report(name, &gated, &replanned).await {
            Ok(report) => report,
            Err(reason) => {
                not_updated(&reason);
                return;
            }
        };
        if let Some(state) = inner
            .live
            .lock()
            .await
            .get_mut(name)
            .and_then(|e| e.guest.as_mut())
        {
            state.editor_file.clone_from(&replanned.editor_file);
        }
        match report.left_files().first() {
            Some((path, why)) if path.ends_with(".code-workspace") => inner.problems.raise(
                Problem::new(
                    key,
                    format!("Puddle could not update the editor's file in {name}"),
                    format!(
                        "{path} was left as it is: {why}. The editor opens it as it is, or \
                         the volume root when it lists no folder, until the file is fixed or \
                         deleted."
                    ),
                )
                .about(name.clone()),
            ),
            _ => inner.problems.clear(&key),
        }
    }

    /// Plans the guest as `guest` says and runs the plan in it. When the hook put the loader of
    /// the shell init back, the user is told, as a boot would.
    async fn run_plan(
        &self,
        name: &WorkspaceName,
        gated: &GatedSandbox<R::Sandbox>,
        guest: &GuestState,
    ) -> Result<(), String> {
        self.run_plan_report(name, gated, guest).await.map(|_| ())
    }

    /// As [`Self::run_plan`], with what the hook said.
    async fn run_plan_report(
        &self,
        name: &WorkspaceName,
        gated: &GatedSandbox<R::Sandbox>,
        guest: &GuestState,
    ) -> Result<puddle_boot::BootReport, String> {
        let kit = &self.inner.kit;
        let inputs = GuestInputs {
            ca: &guest.ca,
            authors: &guest.authors,
            env: &guest.env,
            editor: guest.editor.as_ref(),
            dev_cert: guest.dev_cert.as_ref(),
            root: &guest.root,
            start_folder: guest.start_folder.as_ref(),
            editor_file: guest.editor_file.as_ref(),
        };
        let (plan, _) = kit.plan(&guest.image, &inputs)?;
        let report = kit
            .hook()
            .run(gated.ungated(), &plan)
            .await
            .map_err(|failure| failure.to_string())?;
        self.announce_loader_replaced(name, &report);
        Ok(report)
    }

    fn announce_loader_replaced(&self, name: &WorkspaceName, report: &puddle_boot::BootReport) {
        if crate::shell_init::loader_replaced(&report.replaced_files()) {
            self.inner.events.emit(Event::WorkspaceShellInitRestored {
                workspace: name.clone(),
            });
        }
    }

    /// Looks at every running workspace again (after events were missed).
    pub(crate) async fn all_git_changed(&self) {
        for name in self.inner.injection.running_workspaces() {
            self.git_changed(&name).await;
        }
    }

    /// A change to `name`'s variables: the registry follows (see `Injection::environment_changed`),
    /// and the running guest's login-shell file is rewritten, so a terminal opened from now on
    /// sees the new variables with no restart. A process that is already running keeps what it
    /// has, and so does a sandbox's own environment (it is fixed when the sandbox is made).
    pub(crate) async fn environment_changed(&self, name: &WorkspaceName) {
        if let Some(env) = self.inner.injection.environment_changed(name).await {
            self.rewrite_env(name, env).await;
        }
    }

    fn note_env_missed(&self, name: &WorkspaceName) {
        self.inner
            .env_missed_while_booting
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(name.clone());
    }

    /// Writes `env` into the running guest's login-shell file by running the boot plan again (the
    /// hook is idempotent: only the files that differ change).
    async fn rewrite_env(&self, name: &WorkspaceName, env: GuestEnv) {
        let inner = &self.inner;
        // The miss is recorded under the `live` lock: `start` inserts the entry under the same
        // lock before `settle` reads the record, so a change either finds the entry or is seen by
        // `settle`, never neither.
        let rewrite = {
            let live = inner.live.lock().await;
            let Some(entry) = live.get(name) else {
                // Not in the map yet (the boot hook has not returned, so the boot read the
                // variables before this change and `settle` looks again) or stopped (the next
                // start reads them).
                self.note_env_missed(name);
                return;
            };
            Arc::clone(&entry.rewrite)
        };
        let _one_at_a_time = rewrite.lock().await;
        let running = {
            let live = inner.live.lock().await;
            let running = live
                .get(name)
                .and_then(|e| Some((e.gated.clone()?, e.guest.clone()?)));
            if running.is_none() {
                self.note_env_missed(name);
            }
            running
        };
        let Some((gated, guest)) = running else {
            return;
        };
        if guest.env == env {
            return;
        }
        let replanned = GuestState {
            env: env.clone(),
            ..guest
        };
        match self.run_plan(name, &gated, &replanned).await {
            Ok(()) => {
                if let Some(entry) = inner.live.lock().await.get_mut(name)
                    && let Some(state) = entry.guest.as_mut()
                {
                    state.env = env;
                }
            }
            Err(reason) => {
                tracing::warn!(workspace = %name, %reason, "the variables in the workspace's login shells are not updated");
                inner.events.emit(Event::WorkspaceEnvFailed {
                    workspace: name.clone(),
                    reason: format!(
                        "the change is saved, but new terminals in {name} keep the old variables \
                         until it restarts: {reason}"
                    ),
                });
            }
        }
    }

    /// Writes `name`'s saved shell init to the folder its guest sees, for a start. A folder that
    /// held something else is put right and remembered, so `settle` tells the user.
    fn publish_shell_init(&self, id: &WorkspaceId, name: &WorkspaceName) -> Result<(), String> {
        let published = self.inner.kit.shell_init().publish(id, name)?;
        if published == crate::shell_init::Published::Restored {
            self.inner
                .shell_init_changed_inside
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(name.clone());
        }
        Ok(())
    }

    /// The user saved or removed `name`'s shell init: the folder the running guest sees follows
    /// at once. A workspace that is gone is left alone.
    pub(crate) fn shell_init_changed(&self, name: &WorkspaceName) {
        let id = self
            .state()
            .slots
            .iter()
            .find(|(_, slot)| &slot.record.name == name && !slot.deleting)
            .map(|(id, _)| id.clone());
        let Some(id) = id else { return };
        let key = problem_key("shell-init-copy", name);
        match self.inner.kit.shell_init().refresh(&id, name) {
            Ok(_) => self.inner.problems.clear(&key),
            Err(reason) => {
                tracing::warn!(workspace = %name, %reason, "the workspace's shell init is not updated");
                self.inner.problems.raise(
                    Problem::new(
                        key,
                        format!("Puddle could not update {name}'s shell init"),
                        format!(
                            "{reason}. The workspace keeps the shell init it had; saving it again, \
                             or restarting the workspace, tries again."
                        ),
                    )
                    .about(name.clone()),
                );
            }
        }
    }

    /// After a boot: the image's entrypoint that failed at once becomes a problem on the workspace
    /// (the start goes on: the workspace is usable without it, and a refusal would lock the user
    /// out of the way to look at it). The start cleared the earlier one ([`ReportDesk::begin`]);
    /// one that fails later is the guest agent's report, in the same words.
    fn settle_entrypoint(&self, name: &WorkspaceName, report: &puddle_boot::BootReport) {
        if let Some(failure) = report.entrypoint_failure() {
            tracing::warn!(workspace = %name, %failure, "the image's entrypoint exited at once");
            self.inner.problems.raise(entrypoint_problem(name, failure));
        }
    }

    /// After a boot: tell the user when something inside the workspace had changed its shell init
    /// (the folder, or the file that loads it), and check that the saved script still parses and
    /// prints nothing. A script that fails the check does not stop the start: it is a problem on
    /// the workspace, because a start that refuses over a user's own script would lock them out of
    /// the way to fix it.
    async fn settle_shell_init(&self, name: &WorkspaceName, gated: &GatedSandbox<R::Sandbox>) {
        let inner = &self.inner;
        let folder_changed = inner
            .shell_init_changed_inside
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(name);
        if folder_changed {
            inner.events.emit(Event::WorkspaceShellInitRestored {
                workspace: name.clone(),
            });
        } else {
            self.announce_loader_replaced(name, gated.boot_report());
        }
        let key = problem_key("shell-init", name);
        let saved = inner
            .kit
            .shell_init()
            .saved(name)
            .inspect_err(|reason| {
                tracing::warn!(workspace = %name, %reason, "the shell init was not checked");
            })
            .ok()
            .flatten();
        let Some(script) = saved else {
            inner.problems.clear(&key);
            return;
        };
        match self.run_shell_init_check(gated, &script).await {
            Ok(None) => inner.problems.clear(&key),
            Ok(Some(why)) => inner.problems.raise(
                Problem::new(
                    key,
                    format!("{name}'s shell init needs fixing"),
                    format!(
                        "{why}. The workspace started, but tools that read a shell's output may \
                         fail until you change it on the workspace's Shell init tab."
                    ),
                )
                .about(name.clone()),
            ),
            Err(reason) => {
                tracing::warn!(workspace = %name, %reason, "the shell init was not checked");
                inner.problems.raise(
                    Problem::new(
                        key,
                        format!("Puddle could not check {name}'s shell init"),
                        format!("{reason}. It will be checked again at the next start."),
                    )
                    .about(name.clone()),
                );
            }
        }
    }

    /// Runs `script` once in a throwaway shell in the guest. `Ok(None)` when it parses and prints
    /// nothing, else what is wrong with it.
    async fn run_shell_init_check(
        &self,
        gated: &GatedSandbox<R::Sandbox>,
        script: &str,
    ) -> Result<Option<String>, String> {
        match gated
            .ungated()
            .exec(crate::shell_init::check_request(script))
            .await
        {
            Ok(output) => match crate::shell_init::verdict(&output)? {
                crate::shell_init::Verdict::Quiet => Ok(None),
                crate::shell_init::Verdict::Refused(why) => Ok(Some(why)),
            },
            Err(ComputeError::ExecTimeout { .. }) => {
                Ok(Some(crate::shell_init::timed_out_message()))
            }
            Err(e) => Err(e.to_string()),
        }
    }

    /// Takes back what a create that failed with `reason` left, and returns the reason, with
    /// what could not be taken back added.
    async fn abort_attachment(
        &self,
        attachment: puddle_workspace::Attachment<'_>,
        reason: String,
    ) -> String {
        match attachment.abort(&self.inner.runtime).await {
            Ok(()) => reason,
            Err(e) => {
                tracing::warn!(error = %e, "cleanup after a failed create failed");
                with_leftovers(reason, &[e.to_string()])
            }
        }
    }

    /// Tells the user about stale git locks that could not be cleared, and withdraws the
    /// problem when they were.
    fn report_locks(
        &self,
        name: &WorkspaceName,
        locks: Result<puddle_workspace::LockReport, puddle_workspace::WorkspaceError>,
    ) {
        let key = problem_key("git-locks", name);
        let why = match locks {
            Ok(report) if report.errors.is_empty() => {
                tracing::debug!(workspace = %name, ?report, "after boot");
                self.inner.problems.clear(&key);
                return;
            }
            Ok(report) => report.errors.join("; "),
            Err(e) => e.to_string(),
        };
        self.inner.problems.raise(
            Problem::new(
                key,
                format!("Puddle could not clear the stale git lock files in {name}"),
                format!(
                    "{why}. The workspace started, but git may say \"Another git process seems to \
                     be running\" until the .lock files in the repository's .git folder are \
                     deleted by hand."
                ),
            )
            .about(name.clone()),
        );
    }

    /// After a boot: Puddle's directory and stale locks on the volume, the notice and check for the
    /// shell init, the SSH endpoint, and the handle the lifecycle stops at exit.
    async fn settle(&self, record: &WorkspaceRecord) -> Result<(), String> {
        let inner = &self.inner;
        let (id, name) = (&record.id, &record.name);
        self.progress(name, WorkspaceStep::Syncing, None);
        let gated = {
            let live = inner.live.lock().await;
            live.get(name)
                .and_then(|l| l.gated.clone())
                .ok_or_else(|| format!("{name} is not running"))?
        };
        let locks = inner
            .workspaces
            .after_boot(gated.ungated(), id)
            .await
            .map_err(|e| e.to_string())?;
        self.report_locks(name, locks);
        self.settle_shell_init(name, &gated).await;
        self.settle_entrypoint(name, gated.boot_report());
        self.settle_kubernetes(name, gated.boot_report());
        let authors = inner
            .live
            .lock()
            .await
            .get(name)
            .and_then(|live| live.guest.as_ref().map(|guest| guest.authors.clone()));
        if let Some(authors) = authors {
            self.settle_git_version(name, gated.boot_report(), &authors);
        }
        let root = Layout::new(id).map_err(|e| e.to_string())?.mount().clone();
        self.check_start_folder(name, &gated, &root, record.start_folder.as_ref())
            .await;
        let mount = Layout::new(id).map_err(|e| e.to_string())?.mount().clone();
        let handle = inner
            .runtime
            .get(&name.sandbox_name())
            .await
            .map_err(|e| e.to_string())?;
        if inner.lifecycle.manage(handle, vec![mount]).is_err() {
            return Err("Puddle is shutting down".to_owned());
        }
        // Direct SSH is off unless the user allowed it: then there is no endpoint to connect to
        // and no ssh config entry, however the guest behaves.
        if self.direct_ssh_allowed(name) {
            self.open_ssh(name, gated).await;
        }
        // A change made while the guest booted found no guest to rewrite: look again.
        self.git_changed(name).await;
        let missed = self
            .inner
            .env_missed_while_booting
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(name);
        if missed {
            self.environment_changed(name).await;
        }
        // From here ports can be forwarded, and the pinned forwards come back.
        self.inner.ports.started(name).await;
        Ok(())
    }

    /// The single gate for direct SSH: whether the user allowed it for `name`. Whatever opens an
    /// SSH way into a workspace (the endpoint, and any ssh config entry) must check this first.
    /// It is its override over the global default over off. Settings that cannot be read count as
    /// off, with the reason logged (and shown on the workspace by the API).
    #[must_use]
    pub fn direct_ssh_allowed(&self, name: &WorkspaceName) -> bool {
        match self.direct_ssh_setting(name) {
            Ok(on) => on,
            Err(reason) => {
                tracing::warn!(workspace = %name, %reason, "direct SSH stays off: the settings cannot be read");
                false
            }
        }
    }

    /// What a desktop editor opens in the running workspace `record`, once its ssh entry is in
    /// place: the multi-root file when the editor opens one, else the start folder or the volume
    /// root. Asked again, it writes the entry again, so an entry that could not be written when the
    /// endpoint opened (a config file that was locked) gets another try now, and says why if it
    /// fails again.
    ///
    /// # Errors
    ///
    /// Why the editor cannot be opened, for the user.
    async fn desktop_target(&self, record: &WorkspaceRecord) -> Result<DesktopTarget, String> {
        let name = &record.name;
        match self.direct_ssh_setting(name) {
            Ok(true) => {}
            Ok(false) => {
                return Err("direct SSH is off for this workspace; allow it first".to_owned());
            }
            Err(reason) => {
                tracing::warn!(workspace = %name, %reason, "direct SSH stays off: the settings cannot be read");
                return Err(format!(
                    "the settings cannot be read ({reason}); direct SSH counts as off until they can"
                ));
            }
        }
        let live = self.inner.live.lock().await;
        let entry = live.get(name);
        let Some(endpoint) = entry
            .and_then(|e| e.ssh.as_ref())
            .map(|ssh| ssh.endpoint().path().to_path_buf())
        else {
            return Err(format!(
                "direct SSH has no endpoint in {name}; restart the workspace to try again"
            ));
        };
        let guest = entry.and_then(|e| e.guest.as_ref());
        let (path, is_workspace_file) = match guest {
            Some(guest) => match &guest.editor_file {
                Some(file) => (file.path().clone(), true),
                None => (
                    crate::editor_file::start_path(&guest.root, record.start_folder.as_ref()),
                    false,
                ),
            },
            None => return Err(format!("{name} is not running; start it first")),
        };
        let key = problem_key("ssh", name);
        self.inner.ssh.publish(name, &endpoint).map_err(|reason| {
            self.ssh_not_usable(name, &reason);
            format!("direct SSH is on but the ssh config could not be written: {reason}")
        })?;
        self.inner.problems.clear(&key);
        Ok(DesktopTarget {
            ssh_host: ssh_host(name),
            path: path.as_str().to_owned(),
            is_workspace_file,
        })
    }

    /// Which server the browser editor runs at this start: Microsoft's only when the user chose
    /// it and the recorded consent covers today's terms; anything else, settings that cannot be
    /// read included, is the bundled code-server (a consent is never assumed).
    fn editor_kind(&self) -> ServerKind {
        match crate::settings_read::global(self.inner.settings.as_ref()) {
            Ok(global) => match global.editor_server() {
                ServerChoice::Microsoft => ServerKind::Microsoft {
                    telemetry: global.vscode_server.telemetry(),
                    auto_update: global.vscode_server.auto_update(),
                },
                ServerChoice::CodeServer => {
                    if global.vscode_server.server() == ServerChoice::Microsoft {
                        tracing::warn!(
                            "the editor runs code-server: Microsoft's server is chosen but the consent no longer covers its terms"
                        );
                    }
                    ServerKind::CodeServer
                }
            },
            Err(reason) => {
                tracing::warn!(%reason, "the editor runs code-server: the settings cannot be read");
                ServerKind::CodeServer
            }
        }
    }

    /// The browser editor's reconnection grace for `name`, in seconds: the settings' value, the
    /// default when they cannot be read (the editor is not worth a failed start).
    fn reconnection_grace(&self, name: &WorkspaceName) -> u32 {
        let settings = self.inner.settings.as_ref();
        let read = || {
            let global = crate::settings_read::global(settings)?;
            let own = crate::settings_read::workspace(settings, name)?;
            Ok::<_, String>(resolve(&global, Some(&own)).reconnection_grace.value)
        };
        match read() {
            Ok(grace) => grace.secs(),
            Err(reason) => {
                tracing::warn!(workspace = %name, %reason, "the editor keeps the default reconnection grace: the settings cannot be read");
                ReconnectionGrace::DEFAULT.secs()
            }
        }
    }

    /// Asks the launcher to open `record`'s editor in a window of its own. A launcher with no
    /// window (the default, so `puddle serve`) says the editor opens from the desktop app: the
    /// password is held back from anything but a shell.
    async fn attach_browser(&self, record: &WorkspaceRecord) -> Attached {
        let name = &record.name;
        // No window opens on settings that cannot be read: the user may have tightened what the
        // window does (no clipboard prompt, no zoom keys), and the defaults would undo that.
        let settings = match self.window_settings(name) {
            Ok(settings) => settings,
            Err(reason) => {
                tracing::warn!(workspace = %name, %reason, "no window opens: the settings cannot be read");
                return Attached::not_opened(format!(
                    "the editor window did not open because of this: {reason}; fix the settings file, or delete it to go back to the defaults, then open the editor again"
                ));
            }
        };
        let target = BrowserTarget {
            origin: puddle_front::origin_of(self.inner.front_addr.port(), name),
            front: self.inner.front_addr,
            settings,
            login: self.inner.flavours.of(name).map(|flavour| {
                let login: Arc<dyn EditorLogin> = Arc::new(GuestLogin {
                    guests: Arc::clone(&self.inner.front_guests),
                    ports: self.inner.ports.clone(),
                    workspace: name.clone(),
                });
                // Microsoft's server takes the password file as its connection token;
                // code-server takes it in its login form.
                if flavour == puddle_front::Flavour::Microsoft {
                    EditorSignIn::ConnectionToken(login)
                } else {
                    EditorSignIn::Form(login)
                }
            }),
        };
        match self.inner.launcher.open_browser(record, &target).await {
            Ok(()) => Attached::opened(),
            Err(err) => Attached::not_opened(err.to_string()),
        }
    }

    /// What `name`'s settings say about its window, or why they cannot be read.
    fn window_settings(&self, name: &WorkspaceName) -> Result<Effective, String> {
        let settings = self.inner.settings.as_ref();
        let global = crate::settings_read::global(settings)?;
        let own = crate::settings_read::workspace(settings, name)?;
        Ok(resolve(&global, Some(&own)))
    }

    fn direct_ssh_setting(&self, name: &WorkspaceName) -> Result<bool, String> {
        let settings = self.inner.settings.as_ref();
        let global = crate::settings_read::global(settings)?;
        let own = crate::settings_read::workspace(settings, name)?;
        Ok(resolve(&global, Some(&own)).direct_ssh.value)
    }

    /// Opens the SSH endpoint of a running sandbox and puts its entry in Puddle's ssh config. A
    /// failure is reported to the user ([`Event::WorkspaceSshFailed`] and a problem that lasts
    /// until it works): the workspace runs without direct SSH.
    async fn open_ssh(&self, name: &WorkspaceName, gated: Arc<GatedSandbox<R::Sandbox>>) {
        let inner = &self.inner;
        let listener = match inner.kit.ipc().listen() {
            Ok(listener) => listener,
            Err(e) => {
                self.endpoint_failed(name, &e);
                return;
            }
        };
        let target = Arc::new(ReportingTarget::new(
            gated,
            name.clone(),
            Arc::clone(&inner.events),
        ));
        let ssh = match SshEndpoint::start(listener, target) {
            Ok(ssh) => ssh,
            Err(e) => {
                self.endpoint_failed(name, &e);
                return;
            }
        };
        let endpoint = ssh.endpoint().path().to_path_buf();
        // What is left over: an endpoint this one replaced, or this one when it has no place.
        let leftover = {
            let mut live = inner.live.lock().await;
            // The switch may have gone off while the endpoint was starting: then it has no place.
            match live.get_mut(name).filter(|_| self.direct_ssh_allowed(name)) {
                Some(entry) => {
                    let stale = entry.ssh.replace(ssh);
                    // The entry is written while the lock is held, so a switch turned off
                    // meanwhile (which takes the endpoint under the same lock) removes it
                    // after, never before.
                    self.publish_entry(name, &endpoint);
                    stale
                }
                None => Some(ssh),
            }
        };
        if let Some(leftover) = leftover {
            leftover.close().await;
        }
    }

    /// Puts `name`'s entry in Puddle's ssh config, naming `endpoint`. The caller has checked
    /// [`Self::direct_ssh_allowed`]. A failure is a problem on the workspace: its endpoint is up,
    /// but nothing can name it.
    fn publish_entry(&self, name: &WorkspaceName, endpoint: &std::path::Path) {
        let key = problem_key("ssh", name);
        match self.inner.ssh.publish(name, endpoint) {
            Ok(()) => self.inner.problems.clear(&key),
            Err(reason) => self.ssh_not_usable(name, &reason),
        }
    }

    fn endpoint_failed(&self, name: &WorkspaceName, error: &dyn std::fmt::Display) {
        report_no_endpoint(self.inner.events.as_ref(), name, error);
        self.ssh_not_usable(name, &format!("it has no SSH endpoint ({error})"));
    }

    /// Raises the problem that direct SSH is on for `name` and cannot be used.
    fn ssh_not_usable(&self, name: &WorkspaceName, reason: &str) {
        tracing::warn!(workspace = %name, %reason, "direct SSH is on but not usable");
        self.inner.problems.raise(
            Problem::new(
                problem_key("ssh", name),
                format!("Direct SSH does not work in {name}"),
                format!(
                    "{reason}. The workspace runs, but ssh and VS Code cannot connect to it; \
                     restarting the workspace tries again."
                ),
            )
            .about(name.clone()),
        );
    }

    /// Ends `name`'s SSH way in: the entry first, so nothing names a closed endpoint, then the
    /// endpoint, and the problem about it. An entry that cannot be taken out is a problem that
    /// stays: the file names an endpoint that is closed.
    async fn close_ssh(&self, name: &WorkspaceName, ssh: SshEndpoint) {
        let key = problem_key("ssh", name);
        match self.inner.ssh.withdraw(name) {
            Ok(()) => self.inner.problems.clear(&key),
            Err(reason) => {
                tracing::warn!(workspace = %name, %reason, "the ssh config still names a closed endpoint");
                self.inner.problems.raise(
                    Problem::new(
                        key,
                        format!("Puddle's ssh config still names {name}"),
                        format!(
                            "{reason}. The entry points at an endpoint that is now closed, so \
                             `ssh {}` finds nothing; the file is written again, without the \
                             entry, whenever any workspace's direct SSH starts or stops, and this \
                             note ends when this workspace's direct SSH starts or is turned off.",
                            ssh_host(name)
                        ),
                    )
                    .about(name.clone()),
                );
            }
        }
        ssh.close().await;
    }

    /// Makes every running sandbox's SSH endpoint and ssh config entry match its direct SSH
    /// setting, at once and without a restart.
    async fn apply_direct_ssh(&self) {
        type Running<S> = (WorkspaceName, Option<Arc<GatedSandbox<S>>>, bool);
        let running: Vec<Running<R::Sandbox>> = {
            let live = self.inner.live.lock().await;
            live.iter()
                .filter_map(|(name, entry)| {
                    Some((
                        name.clone(),
                        Some(entry.gated.clone()?),
                        entry.ssh.is_some(),
                    ))
                })
                .collect()
        };
        for (name, gated, open) in running {
            let allowed = self.direct_ssh_allowed(&name);
            match (allowed, open, gated) {
                (true, false, Some(gated)) => self.open_ssh(&name, gated).await,
                (false, true, _) => {
                    let ssh = self
                        .inner
                        .live
                        .lock()
                        .await
                        .get_mut(&name)
                        .and_then(|entry| entry.ssh.take());
                    if let Some(ssh) = ssh {
                        self.close_ssh(&name, ssh).await;
                    }
                }
                (false, false, _) => self.inner.problems.clear(&problem_key("ssh", &name)),
                _ => {}
            }
        }
    }

    /// Stops what a failed boot or a finished stop leaves: the SSH endpoint and the lifecycle's
    /// handle. The route stays (the sandbox may start again); `drop_route` ends it too.
    async fn quiesce(&self, name: &WorkspaceName, drop_route: bool) {
        self.inner.lifecycle.release(&name.sandbox_name());
        // Its forwards close; the pinned ones come back at the next start.
        self.inner.ports.stopped(name).await;
        self.inner.flavours.stopped(name);
        // The CA belongs to one start: a stopped or failed sandbox decrypts nothing.
        self.inner.injection.end(name);
        self.inner.problems.clear(&problem_key("ssh", name));
        GitProblem::clear_all(&self.inner.problems, name);
        self.inner.problems.clear(&kubernetes_key(name));
        let (ssh, route) = {
            let mut live = self.inner.live.lock().await;
            if drop_route {
                match live.remove(name) {
                    Some(entry) => (entry.ssh, Some(entry.route)),
                    None => (None, None),
                }
            } else {
                match live.get_mut(name) {
                    Some(entry) => {
                        entry.gated = None;
                        entry.guest = None;
                        (entry.ssh.take(), None)
                    }
                    None => (None, None),
                }
            }
        };
        if let Some(ssh) = ssh {
            self.close_ssh(name, ssh).await;
        }
        if let Some(route) = route {
            route.shutdown().await;
        }
    }

    /// Takes a sandbox that failed after it booted all the way down again, and returns `reason`
    /// with whatever could not be taken down added.
    async fn undo_boot(
        &self,
        record: &WorkspaceRecord,
        remove: bool,
        created_volume: bool,
        reason: String,
    ) -> String {
        let inner = &self.inner;
        let mut leftovers = Vec::new();
        let name = &record.name;
        let gated = inner
            .live
            .lock()
            .await
            .get(name)
            .and_then(|l| l.gated.clone());
        if let Some(gated) = gated
            && let Err(e) = gated.stop().await
        {
            tracing::warn!(workspace = %name, error = %e, "stopping a failed workspace failed");
            leftovers.push(format!("the machine {name} may still be running ({e})"));
        }
        self.quiesce(name, remove).await;
        self.inner.reports.end(name);
        if remove {
            match inner.runtime.remove(&name.sandbox_name()).await {
                Ok(()) | Err(ComputeError::NotFound { .. }) => {}
                Err(e) => {
                    tracing::warn!(workspace = %name, error = %e, "removing a failed sandbox failed");
                    leftovers.push(format!("the sandbox {name} was not removed ({e})"));
                }
            }
            inner.workspaces.sandbox_removed(&name.sandbox_name());
        }
        if created_volume
            && let Err(e) = inner.runtime.remove_volume(&record.id.volume_name()).await
        {
            tracing::warn!(workspace = %record.id, error = %e, "removing a new volume failed");
            leftovers.push(format!(
                "the new volume {} was not removed ({e})",
                record.id.volume_name()
            ));
        }
        with_leftovers(reason, &leftovers)
    }

    async fn create_work(&self, record: WorkspaceRecord) -> Result<(), String> {
        self.inner.reports.begin(&record.name);
        let mut created_volume = false;
        self.boot_new(&record, &mut created_volume, true, true)
            .await?;
        if let Err(reason) = self.settle(&record).await {
            return Err(self.undo_boot(&record, true, created_volume, reason).await);
        }
        let gated = self.gated(&record.name).await?;
        if let Err(reason) = self.clone_all(&record, gated.ungated()).await {
            return Err(self.undo_boot(&record, true, created_volume, reason).await);
        }
        // Nobody has worked in it yet, so undoing it now loses nothing; keeping it would leave
        // a workspace the next start takes for an unfinished create.
        if let Err(e) = self.mark_created(&record.id) {
            let reason = format!("{e}; the new workspace was removed again; {SAVE_HINT}");
            return Err(self.undo_boot(&record, true, created_volume, reason).await);
        }
        self.refresh_checkouts_later(&record.id).await;
        Ok(())
    }

    /// Clones the repositories of a new workspace one at a time in list order, then runs one
    /// `sync`. A clone git refuses fails only its repository: a background problem names it and
    /// the next one starts (git removes the folder of its own failed clone). Anything else (the
    /// guest gone, the sync failing) fails the create, which nobody has worked in yet.
    async fn clone_all(
        &self,
        record: &WorkspaceRecord,
        sandbox: &R::Sandbox,
    ) -> Result<(), String> {
        let workspaces = &self.inner.workspaces;
        for repository in &record.repositories {
            self.progress_cloning(&record.name, repository);
            let key = clone_problem_key(&record.name, &repository.directory);
            let cloned = workspaces
                .clone_checkout(
                    sandbox,
                    &record.id,
                    repository.url.as_str(),
                    &repository.directory,
                    None,
                )
                .await;
            match cloned {
                Ok(_) => self.inner.problems.clear(&key),
                Err(puddle_workspace::WorkspaceError::Clone { reason, .. }) => {
                    tracing::warn!(workspace = %record.name, url = %repository.url, %reason, "a repository of a new workspace was not cloned");
                    self.raise_clone_problem(record, repository, &reason, true);
                }
                Err(other) => return Err(other.to_string()),
            }
        }
        if record.repositories.is_empty() {
            return Ok(());
        }
        workspaces
            .sync(sandbox, &record.id)
            .await
            .map_err(|e| e.to_string())
    }

    async fn gated(&self, name: &WorkspaceName) -> Result<Arc<GatedSandbox<R::Sandbox>>, String> {
        self.inner
            .live
            .lock()
            .await
            .get(name)
            .and_then(|l| l.gated.clone())
            .ok_or_else(|| format!("{name} is not running"))
    }

    async fn start_work(&self, record: WorkspaceRecord) -> Result<(), String> {
        let inner = &self.inner;
        let name = &record.name;
        self.progress(name, WorkspaceStep::Starting, None);
        inner.problems.clear(&problem_key("forced-stop", name));
        inner.reports.begin(name);
        let exists = inner
            .runtime
            .list()
            .await
            .map_err(|e| e.to_string())?
            .iter()
            .any(|s| s.name == name.as_str());
        let route_alive = inner.live.lock().await.contains_key(name);
        let restarted = exists && route_alive && self.restart_in_place(&record).await?;
        if exists && route_alive && !restarted {
            // The root disk goes with the old sandbox (home directory, installed packages); the
            // volume with the work stays. Say so: this is Puddle's own doing.
            self.progress(
                name,
                WorkspaceStep::Starting,
                Some(
                    "the workspace's variables changed, so its sandbox is made again; \
                     the volume with your work is kept, what was installed outside it is not"
                        .to_owned(),
                ),
            );
        }
        if !restarted {
            if exists {
                inner
                    .runtime
                    .remove(&name.sandbox_name())
                    .await
                    .map_err(|e| e.to_string())?;
                inner.workspaces.sandbox_removed(&name.sandbox_name());
            }
            // A route left from an earlier boot belongs to a sandbox that no longer exists.
            self.quiesce(name, true).await;
            let mut created_volume = false;
            self.boot_new(&record, &mut created_volume, false, false)
                .await?;
        }
        if let Err(reason) = self.settle(&record).await {
            return Err(self.undo_boot(&record, false, false, reason).await);
        }
        self.refresh_checkouts_later(&record.id).await;
        Ok(())
    }

    /// Starts the stopped sandbox again. `Ok(false)` when it did not: the workspace's variables
    /// changed since the sandbox was made and a sandbox keeps the environment it was made with, so
    /// the caller makes a new one on the same volume (the volume holds the work, not the sandbox).
    async fn restart_in_place(&self, record: &WorkspaceRecord) -> Result<bool, String> {
        let inner = &self.inner;
        let name = &record.name;
        let image = ImageRef::new(&record.image).map_err(|e| e.to_string())?;
        let config = self.image_config(&image).await?;
        self.publish_shell_init(&record.id, name)?;
        let (plan, _, guest) = self.plan_guest_for(record, config).await?;
        let made_with = inner
            .live
            .lock()
            .await
            .get(name)
            .map(|live| live.spec_env.clone());
        if made_with.is_some_and(|env| env != guest.env) {
            inner.injection.end(name);
            return Ok(false);
        }
        let started = self.start_planned(record, &plan).await;
        match started {
            Ok(gated) => {
                inner.flavours.running(name, guest.editor.as_ref());
                if let Some(live) = inner.live.lock().await.get_mut(name) {
                    live.gated = Some(Arc::new(gated));
                    live.guest = Some(guest);
                }
                Ok(true)
            }
            Err(reason) => {
                inner.injection.end(name);
                Err(reason)
            }
        }
    }

    /// Starts the stopped sandbox of `record` through the boot hook with `plan`.
    async fn start_planned(
        &self,
        record: &WorkspaceRecord,
        plan: &puddle_boot::BootPlan,
    ) -> Result<GatedSandbox<R::Sandbox>, String> {
        let inner = &self.inner;
        let name = &record.name;
        let gate = inner
            .live
            .lock()
            .await
            .get(name)
            .map(|l| l.gate.clone())
            .ok_or_else(|| format!("{name} has no route"))?;
        // The memory setting applies at the next start (ADR 0006 and the memory setting).
        inner
            .runtime
            .set_memory(&name.sandbox_name(), record.memory)
            .await
            .map_err(|e| e.to_string())?;
        // The cpus setting applies at the next start, like the memory.
        inner
            .runtime
            .set_cpus(&name.sandbox_name(), self.cpus(name))
            .await
            .map_err(|e| e.to_string())?;
        inner
            .kit
            .hook()
            .start(&inner.runtime, &name.sandbox_name(), plan, &gate)
            .await
            .map_err(|e| boot_message(&e))
    }

    async fn stop_work(&self, record: WorkspaceRecord) -> Result<(), String> {
        let name = &record.name;
        // The windows go first, so the editor can disconnect and the guest end its extension hosts
        // before the stop (best effort: the close is asked for, not waited for).
        self.inner.launcher.close_browser(name).await;
        let gated = self.gated(name).await?;
        self.progress(name, WorkspaceStep::Reclaiming, None);
        // A failed trim only costs disk space; the VM still stops, and the user is told.
        let trim_key = problem_key("trim", name);
        match self
            .inner
            .workspaces
            .trim(gated.ungated(), &record.id)
            .await
        {
            Ok(_) => self.inner.problems.clear(&trim_key),
            Err(e) => {
                tracing::warn!(workspace = %record.id, error = %e, "trim before stop failed");
                self.inner.problems.raise(
                    Problem::new(
                        trim_key,
                        format!("{name}'s disk was not trimmed when it stopped"),
                        format!(
                            "{e}. The workspace stopped normally. Its disk file stays larger \
                             than the data in it until a later stop, or Reclaim space, trims it."
                        ),
                    )
                    .about(name.clone()),
                );
            }
        }
        self.progress(name, WorkspaceStep::Stopping, None);
        gated.stop().await.map_err(|e| e.to_string())?;
        if gated.ungated().stopped_by_force() {
            self.inner.problems.raise(
                Problem::new(
                    problem_key("forced-stop", name),
                    format!("{name} did not shut down in time and was ended by force"),
                    "Files it had not written to its disk yet may be missing. Start it again \
                     and check the repositories with git status.",
                )
                .about(name.clone()),
            );
        }
        self.quiesce(name, false).await;
        self.inner.reports.end(name);
        Ok(())
    }

    /// Deletes what the browser editor kept for a deleted workspace, so one made later under the
    /// same name starts clean: the front's half of Microsoft's secret key and the windows' browser
    /// data. A failure is a notice (the data stays and the name would inherit it).
    async fn forget_browser_state(&self, name: &WorkspaceName) {
        let inner = &self.inner;
        let key = problem_key("browser-data", name);
        let mut failed = Vec::new();
        // The windows first: one still open could mint the key again after it was forgotten.
        if let Err(e) = inner.launcher.forget_browser(name).await {
            failed.push(e.to_string());
        }
        if let Err(e) = inner.front_halves.forget(name) {
            failed.push(format!("the editor's secret key could not be deleted: {e}"));
        }
        if failed.is_empty() {
            inner.problems.clear(&key);
        } else {
            inner.problems.raise(Problem::new(
                key,
                format!("{name}'s browser editor data was not deleted"),
                format!(
                    "{}. A workspace made later with the name {name} would find it; close the \
                     editor windows and delete its folders under webview and front in Puddle's \
                     data folder.",
                    failed.join("; ")
                ),
            ));
        }
    }

    async fn delete_work(
        &self,
        record: WorkspaceRecord,
        report: puddle_workspace::DeleteReport,
    ) -> Result<(), String> {
        let inner = &self.inner;
        self.progress(&record.name, WorkspaceStep::Checking, None);
        inner.launcher.close_browser(&record.name).await;
        self.progress(&record.name, WorkspaceStep::Removing, None);
        inner
            .workspaces
            .delete(&inner.runtime, &record.id, &report.confirm())
            .await
            .map_err(|e| e.to_string())?;
        self.quiesce(&record.name, true).await;
        self.inner.reports.forget(&record.name);
        // What the workspace held in Puddle's settings goes with it: its rules, its identities,
        // its environment and the values of its own secrets.
        inner.injection.forget(&record.name).await;
        inner.kit.shell_init().forget(&record.id);
        inner
            .problems
            .clear_prefix(&format!("{}:", problem_key("clone", &record.name)));
        self.forget_browser_state(&record.name).await;
        Ok(())
    }
}

/// Why a boot failed, for the user.
fn boot_message(e: &BootError) -> String {
    e.to_string()
}

impl<R: Runtime + Clone> WorkspaceService for HostWorkspaces<R> {
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
            let id = WorkspaceId::new(new.name.as_str()).map_err(|e| {
                WorkspaceError::Invalid(format!("this name can't be a workspace: {e}"))
            })?;
            let mut record = WorkspaceRecord::new(id.clone(), new.name.clone(), new.entries());
            record.start_folder.clone_from(&new.start_folder);
            record.image = new
                .image
                .as_ref()
                .map_or_else(|| DEFAULT_IMAGE.to_owned(), |i| i.as_str().to_owned());
            record.memory = match new.memory {
                Some(memory) => memory,
                None => self.default_memory().map_err(|reason| {
                    WorkspaceError::Conflict(format!(
                        "{reason}, so Puddle can't tell how much memory the new workspace gets; \
                         fix or reset the settings, or choose a memory size for it"
                    ))
                })?,
            };
            record.created_at = self.inner.clock.now_ms();
            record.disk_size_mib = u64::from(self.inner.workspaces.config().default_size.as_mib());
            record.disk_used_mib = None;
            record.busy = Some(Operation::Creating);
            // A volume of this name that the list does not name holds someone's work (reconcile
            // keeps it); a create would clone into the old checkout. Refuse before anything exists.
            let volume = id.volume_name();
            if self
                .inner
                .runtime
                .volume(&volume)
                .await
                .map_err(|e| {
                    WorkspaceError::Unavailable(format!(
                        "cannot check for an existing volume {volume}: {e}"
                    ))
                })?
                .is_some()
            {
                return Err(WorkspaceError::Conflict(format!(
                    "a volume named {volume} already exists, left from an earlier workspace, and may hold its work; \
                     {} was not created. Choose another name; Puddle does not reuse or delete that volume",
                    new.name
                )));
            }
            {
                let mut state = self.state();
                if state.closed {
                    return Err(WorkspaceError::Unavailable(
                        "Puddle is shutting down".to_owned(),
                    ));
                }
                if state.slots.contains_key(&id)
                    || puddle_api::label_taken(
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
                state.slots.insert(
                    id.clone(),
                    Slot {
                        record: record.clone(),
                        creating: true,
                        deleting: false,
                        volume_missing: false,
                    },
                );
                if let Err(e) = self.persist(&state) {
                    state.slots.remove(&id);
                    return Err(WorkspaceError::Unavailable(format!(
                        "{} was not created: {e}; {SAVE_HINT}",
                        new.name
                    )));
                }
            }
            // The name is reserved and the list saved: set up what the clones need first.
            git.set_up(&new.name, &new.repositories).await;
            self.status_event(&record);
            let this = self.clone();
            let work = record.clone();
            self.run(&record, Operation::Creating, async move {
                this.create_work(work).await
            })
            .await;
            Ok(record)
        })
    }

    fn start<'a>(
        &'a self,
        id: &'a WorkspaceId,
    ) -> BoxFuture<'a, Result<WorkspaceRecord, WorkspaceError>> {
        Box::pin(async move {
            let record = self.accept(
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
            self.status_event(&record);
            let this = self.clone();
            let work = record.clone();
            self.run(&record, Operation::Starting, async move {
                this.start_work(work).await
            })
            .await;
            Ok(record)
        })
    }

    fn stop<'a>(
        &'a self,
        id: &'a WorkspaceId,
    ) -> BoxFuture<'a, Result<WorkspaceRecord, WorkspaceError>> {
        Box::pin(async move {
            let record = self.accept(
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
            self.inner.reports.pause(&record.name);
            self.status_event(&record);
            let this = self.clone();
            let work = record.clone();
            self.run(&record, Operation::Stopping, async move {
                this.stop_work(work).await
            })
            .await;
            Ok(record)
        })
    }

    fn reclaim<'a>(
        &'a self,
        id: &'a WorkspaceId,
    ) -> BoxFuture<'a, Result<WorkspaceRecord, WorkspaceError>> {
        Box::pin(async move {
            let record = self.accept(
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
            let this = self.clone();
            let work = record.clone();
            self.run(&record, Operation::Reclaiming, async move {
                this.progress(&work.name, WorkspaceStep::Reclaiming, None);
                this.inner
                    .workspaces
                    .reclaim_space(&this.inner.runtime, &work.id)
                    .await
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            })
            .await;
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
            let report = self
                .inner
                .workspaces
                .check_delete(&self.inner.runtime, id)
                .await
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
            let record = self.accept(
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
            // The check runs before the call returns, so a refusal reaches the caller.
            let checked = self
                .inner
                .workspaces
                .check_delete(&self.inner.runtime, id)
                .await;
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
                Err(e) => {
                    if let Some(slot) = self.state().slots.get_mut(id) {
                        slot.record.busy = None;
                        slot.deleting = false;
                    }
                    return Err(e);
                }
            };
            let this = self.clone();
            let work = record.clone();
            self.run(&record, Operation::Deleting, async move {
                this.delete_work(work, report).await
            })
            .await;
            Ok(record)
        })
    }

    fn rename<'a>(
        &'a self,
        id: &'a WorkspaceId,
        display_name: Option<DisplayName>,
    ) -> BoxFuture<'a, Result<WorkspaceRecord, WorkspaceError>> {
        Box::pin(async move {
            let mut state = self.state();
            if state.closed {
                return Err(WorkspaceError::Unavailable(
                    "Puddle is shutting down".to_owned(),
                ));
            }
            puddle_api::check_rename(
                state.slots.values().map(|s| &s.record),
                id,
                display_name.as_ref(),
            )?;
            let slot = state.slots.get_mut(id).ok_or_else(|| not_found(id))?;
            let before = std::mem::replace(&mut slot.record.display_name, display_name);
            if let Err(e) = self.persist(&state) {
                // Nothing changes when the list can't be saved, so a restart doesn't undo a
                // rename the user was told about.
                if let Some(slot) = state.slots.get_mut(id) {
                    slot.record.display_name = before;
                }
                return Err(WorkspaceError::Unavailable(format!(
                    "the workspace was not renamed: {e}; {SAVE_HINT}"
                )));
            }
            state
                .slots
                .get(id)
                .map(|s| s.record.clone())
                .ok_or_else(|| not_found(id))
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
                    self.inner.problems.clear(&problem_key(kind, &record.name));
                }
            }
            Ok(StartFolderChange::new(record, exists))
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
                    Err(reason) => Attached::not_opened(reason),
                    Ok(target) => match self.inner.launcher.open_desktop(&record, &target).await {
                        Ok(()) => Attached::opened(),
                        Err(err) => Attached::not_opened(err.to_string()),
                    },
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
            let list = self.inner.injection.kept_logins(&record.name).await;
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
            match self
                .inner
                .injection
                .forget_login(&record.name, service)
                .await
            {
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
                .get(&record.name)
                .and_then(|live| live.gated.clone());
            let Some(gated) = running else {
                return Ok(ShellInitCheck::NotRun(format!(
                    "{} is not running; Puddle checks the script when it starts",
                    record.name
                )));
            };
            match self.run_shell_init_check(&gated, script).await {
                Ok(None) => Ok(ShellInitCheck::Passed),
                Ok(Some(why)) => Ok(ShellInitCheck::Refused(why)),
                Err(reason) => Err(WorkspaceError::Unavailable(reason)),
            }
        })
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
        directory: &'a CheckoutDir,
        git: &'a dyn GitSetup,
    ) -> BoxFuture<'a, Result<WorkspaceRecord, WorkspaceError>> {
        Box::pin(async move { self.clone_entry_again(id, directory, git).await })
    }

    fn remove_repository<'a>(
        &'a self,
        id: &'a WorkspaceId,
        directory: &'a CheckoutDir,
    ) -> BoxFuture<'a, Result<WorkspaceRecord, WorkspaceError>> {
        Box::pin(async move { self.remove_entry(id, directory) })
    }

    fn settings_changed(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move { self.apply_direct_ssh().await })
    }
}

/// The workspace ids a book entry set names, for the inventory reconcile works from. An entry
/// whose create never finished is left out: reconcile removes its volume.
pub(crate) fn known_ids(stored: &[Stored]) -> BTreeSet<WorkspaceId> {
    stored
        .iter()
        .filter(|s| !s.creating)
        .map(|s| s.id.clone())
        .collect()
}

/// One kept login as the API lists it.
fn kept_login_row(login: &puddle_logins::KeptLogin) -> KeptLogin {
    KeptLogin {
        service: login.profile.to_owned(),
        name: login.name.to_owned(),
        since: login.since_ms,
    }
}

#[cfg(test)]
mod kept_login_rows {
    use super::*;

    #[test]
    fn a_row_carries_the_short_name_the_display_name_and_the_time() {
        let row = kept_login_row(&puddle_logins::KeptLogin {
            profile: "claude",
            name: "Claude Code",
            since_ms: Some(1_761_480_000_000),
        });
        assert_eq!(
            row,
            KeptLogin {
                service: "claude".to_owned(),
                name: "Claude Code".to_owned(),
                since: Some(1_761_480_000_000),
            }
        );
    }
}

#[cfg(test)]
mod no_link_words {
    use super::*;

    #[test]
    fn a_running_workspace_whose_agent_is_late_is_not_called_stopped_and_the_user_is_not_blamed() {
        let acme = WorkspaceName::new("acme").unwrap();
        assert_eq!(
            no_link_message(&acme, NoLink::NotRunning),
            "acme is not running; start it first"
        );
        let late = no_link_message(&acme, NoLink::NotConnected);
        assert!(late.starts_with("acme is running"), "{late}");
        assert!(late.contains("open the editor again in a moment"), "{late}");
    }
}
