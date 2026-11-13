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

use crate::agent_reports::{ReportDesk, clear_no_sudo, entrypoint_problem, no_sudo_problem};
use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::ops::{Deref, DerefMut};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use futures_util::future::BoxFuture;
use puddle_agent_proto::editor::{EditorServer, NEEDS_GLIBC, ServerKind};
use puddle_agent_proto::forward::OpenError;
use puddle_agent_proto::host_control::{HostMessage, HostReply};
use puddle_api::{
    AttachMode, Attached, BrowserTarget, DEFAULT_IMAGE, DeleteCheck, DesktopTarget, DisplayName,
    EditorLogin, EditorSignIn, GitSetup, KeptLogin, KeptLogins, LaunchError, Launcher, Listing,
    LocalSource, NewWorkspace, OpenTarget, Operation, RepoFindings, Repository, ShellInitCheck,
    StartFolder, StartFolderChange, WorkspaceError, WorkspaceRecord, WorkspaceService,
};
use puddle_api::{EndReason, TerminalError, TerminalLink, TerminalService, TerminalSize};
use puddle_boot::{BootError, Gate, GatedSandbox};
use puddle_ca::CaCertificate;
use puddle_compute::{
    ComputeError, DiskSize, ExecRequest, ImageConfig, RootDiskGrowth, Runtime, Sandbox as _,
    StartRefusal,
};
use puddle_lifecycle::Lifecycle;
use puddle_ports::{NoLink, Ports};
use puddle_proxy::Route;
use puddle_settings::{Effective, ReconnectionGrace, ServerChoice, WorkspaceSettings, resolve};
use puddle_ssh::SshEndpoint;
use puddle_store::{Actor, Clock, NewWorkspaceOp, Store, WorkspaceOpKind, WorkspaceOpOutcome};
use puddle_types::{
    CheckoutDir, CpuCount, DiskKind, EntryId, Event, EventSink, GuestEnv, GuestFile, GuestPath,
    ImageRef, MemoryMib, Problem, Problems, SessionUser, WorkspaceId, WorkspaceStatus,
    WorkspaceStep,
};

use crate::ssh_config::{SshSetup, ssh_host};
use crate::ssh_target::{ReportingTarget, report_no_endpoint};
use puddle_workspace::{Findings, Import, Layout, LocalCheckout, Workspaces};
use tokio::sync::watch;
use tokio::task::JoinSet;

use crate::HostError;
use crate::boot::{BootKit, GuestInputs, root_disk_size};
use crate::files::{Stored, WorkspaceBook};
use crate::front::Flavours;
use crate::git_hosts::{
    Authors, GitProblem, SshListing, authors, credentials_problem, left_out_problem,
    never_decrypted, old_git_problem,
};
use crate::injection::Injection;
use crate::kubernetes::{kubernetes_key, kubernetes_problem};
use crate::merge_notice::{merge_notice_prefix, settle_merge_notices};

mod repositories;

pub(crate) mod orphans;

impl<R: Runtime + Clone> crate::editor_updates::Builds for HostWorkspaces<R> {
    fn on_microsoft_server(&self) -> Vec<WorkspaceId> {
        self.inner.flavours.on_microsoft()
    }

    async fn switch(
        &self,
        workspace: &WorkspaceId,
        connected_windows: usize,
    ) -> Result<bool, SwitchEditorBuildError> {
        self.switch_editor_build(workspace, connected_windows).await
    }
}

impl<R: Runtime + Clone> crate::changes::Changes for HostWorkspaces<R> {
    fn decrypt_changed(&self, workspace: &WorkspaceId) -> Option<puddle_store::WorkspaceGit> {
        Self::decrypt_changed(self, workspace)
    }

    async fn rewrite_authors(&self, workspace: WorkspaceId) {
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

    async fn environment_changed(&self, workspace: WorkspaceId) {
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

    fn shell_init_changed(&self, workspace: WorkspaceId) -> impl Future<Output = ()> + Send {
        Self::shell_init_changed(self, &workspace);
        std::future::ready(())
    }

    fn all_shell_inits_changed(&self) -> impl Future<Output = ()> + Send {
        let names: Vec<WorkspaceId> = self
            .state()
            .slots
            .values()
            .map(|slot| slot.record.id.clone())
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
pub type RewriteGate = Arc<dyn Fn(&WorkspaceId) -> BoxFuture<'static, Box<dyn Send>> + Send + Sync>;

/// What the service is built from.
pub(crate) struct Parts<R: Runtime + Clone> {
    pub(crate) runtime: R,
    pub(crate) workspaces: Workspaces,
    pub(crate) lifecycle: Arc<Lifecycle<R>>,
    pub(crate) kit: BootKit,
    pub(crate) events: Arc<dyn EventSink>,
    pub(crate) clock: Arc<dyn Clock>,
    pub(crate) settings: Arc<dyn puddle_api::SettingsRepo>,
    /// Where the audit log is written.
    pub(crate) store: Arc<Store>,
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
    /// Where new workspace ids come from.
    pub(crate) ids: Arc<dyn puddle_api::IdSource>,
    /// The folder of the login markers, which a new id must not find one of its own in.
    pub(crate) login_markers: std::path::PathBuf,
    /// The free space of the drive that holds Puddle's data: the one a root disk grows into.
    pub(crate) space: Arc<dyn crate::Space>,
}

struct Slot {
    record: WorkspaceRecord,
    creating: bool,
    /// Being deleted. The saved list keeps the entry marked until the delete has finished, so
    /// a restart in the middle of it can tell what happened from the volume: gone, the delete
    /// went through and the next start finishes the cleanup; still there, the workspace stays.
    deleting: bool,
    /// The start that is running was refused because the volume is gone; `conclude` turns it
    /// into the `volume_missing` state instead of `crashed`.
    volume_missing: bool,
    /// What `conclude` adds to the audit record of the delete that is running: set when the
    /// workspace's volume was already gone.
    delete_note: Option<&'static str>,
}

impl Slot {
    fn new(record: WorkspaceRecord, creating: bool) -> Self {
        Self {
            record,
            creating,
            deleting: false,
            volume_missing: false,
            delete_note: None,
        }
    }
}

/// What the runtime holds, by name, when a create draws an id.
struct RuntimeNames {
    volumes: BTreeSet<String>,
    sandboxes: BTreeSet<String>,
}

/// The lock on the workspace list. Dropping it releases the lock first and then writes the
/// names its holder queued, so no database write ever happens under the lock (the fields drop
/// in the order they are declared).
struct StateGuard<'a, R: Runtime + Clone> {
    guard: MutexGuard<'a, State>,
    _then: WriteNames<'a, R>,
}

impl<R: Runtime + Clone> Deref for StateGuard<'_, R> {
    type Target = State;
    fn deref(&self) -> &State {
        &self.guard
    }
}

impl<R: Runtime + Clone> DerefMut for StateGuard<'_, R> {
    fn deref_mut(&mut self) -> &mut State {
        &mut self.guard
    }
}

/// Writes the queued names to the store when dropped.
struct WriteNames<'a, R: Runtime + Clone>(&'a HostWorkspaces<R>);

impl<R: Runtime + Clone> Drop for WriteNames<'_, R> {
    fn drop(&mut self) {
        let queued = !self
            .0
            .inner
            .names_pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_empty();
        if queued {
            self.0.write_noted_names();
        }
    }
}

struct State {
    slots: BTreeMap<WorkspaceId, Slot>,
    /// The ids of deleted workspaces: no new workspace gets one.
    retired: BTreeSet<WorkspaceId>,
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
    /// This start's SSH host keys and the names the guest lists them under now; `None` when Git
    /// over SSH is off.
    ssh: Option<SshListing>,
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
    /// The user sessions, clones and checks run as until the next start: the setting as of this
    /// start (a later change waits for the next one).
    session_user: SessionUser,
}

/// How long a terminal opened while its workspace starts waits for the boot hook.
const TERMINAL_BOOT_WAIT: Duration = Duration::from_secs(90);
/// How often the wait looks again.
const TERMINAL_BOOT_POLL: Duration = Duration::from_millis(250);

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
    /// The settings routes' lock, shared so a delete's removal of a settings file never falls
    /// between a settings change's read and its write.
    settings_lock: Arc<tokio::sync::Mutex<()>>,
    store: Arc<Store>,
    launcher: Arc<dyn Launcher>,
    ssh: Arc<SshSetup>,
    /// The terminals open now, ended with their reason when the workspace stops.
    terminals: Arc<crate::terminals::Hub>,
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
    ids: Arc<dyn puddle_api::IdSource>,
    login_markers: std::path::PathBuf,
    space: Arc<dyn crate::Space>,
    state: Mutex<State>,
    /// Names waiting to be written to the store; see [`HostWorkspaces::note_names`].
    names_pending: Mutex<BTreeMap<WorkspaceId, String>>,
    /// Held while the queue is written, so writes never reorder; never held with `state`.
    names_writing: Mutex<()>,
    live: tokio::sync::Mutex<BTreeMap<WorkspaceId, Live<R>>>,
    /// The tasks the service owns: operations, maintenance checks and downloads. A plain mutex,
    /// so a request never awaits between marking a workspace busy and spawning the task that
    /// clears the mark.
    tasks: Mutex<JoinSet<()>>,
    running: watch::Sender<usize>,
    /// Whether the image the delete check boots is in the cache; see [`HostWorkspaces::maintenance_ready`].
    maintenance_image: watch::Sender<Warmth>,
    /// Workspaces whose start found their shell-init folder changed from inside, until `settle`
    /// has said so (with what the boot hook found, as one notice).
    shell_init_changed_inside: Mutex<BTreeSet<WorkspaceId>>,
    /// Workspaces whose variables changed while their guest was booting: `settle` applies the
    /// change to the guest once it runs.
    env_missed_while_booting: Mutex<BTreeSet<WorkspaceId>>,
    /// The last listing of each workspace's checkouts; see [`repositories::CachedListing`].
    listings: Mutex<BTreeMap<WorkspaceId, repositories::CachedListing>>,
    /// The disks the host has already warned about, until they drop below the threshold again.
    disk_warned: Mutex<BTreeSet<(WorkspaceId, DiskKind)>>,
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
/// How the room a root-disk grow adds compares with the free space of the drive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Room {
    /// Less than half of what is free.
    Plenty,
    /// More than half of what is free, but it fits: allowed, with a warning.
    Tight,
    /// More than is free: refused.
    TooBig,
}

/// Judges `added` bytes of room against `free` bytes free.
fn room(added: u64, free: u64) -> Room {
    if added > free {
        Room::TooBig
    } else if added > free / 2 {
        Room::Tight
    } else {
        Room::Plenty
    }
}

/// `name` as the user reads it, or `id` for a workspace the list no longer holds.
fn name_or_id(name: Option<&DisplayName>, id: &WorkspaceId) -> String {
    name.map_or_else(|| id.to_string(), |name| name.as_str().to_owned())
}

/// The title of the problem about the editor's file in `record`'s workspace.
fn editor_file_title(record: &WorkspaceRecord) -> String {
    format!(
        "Puddle could not update the editor's file in {}",
        record.label()
    )
}

fn problem_key(kind: &str, name: &WorkspaceId) -> String {
    format!("{kind}:{name}")
}

/// The key of the problem about one repository of a workspace that could not be cloned.
fn clone_problem_key(name: &WorkspaceId, entry: &EntryId) -> String {
    format!("{}:{entry}", problem_key("clone", name))
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

/// How long a delete or its check waits for the image it boots to be downloaded before it asks
/// the user to try again: under the API's 30 s request limit, so the answer is ours and not a 408.
const MAINTENANCE_WAIT: Duration = Duration::from_secs(15);

/// Where the download of the image the delete check boots stands.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Warmth {
    /// Not asked for yet.
    Cold,
    /// A task of the service is pulling it.
    Warming,
    /// In the cache.
    Ready,
    /// The last pull failed (the log says why).
    Failed,
}

/// Waits up to `within` for the download in `warmth` to end. A download that failed is no reason
/// to stop: the check asks for the image itself and says why it can't have it, at once.
async fn wait_until_downloaded(
    warmth: &mut watch::Receiver<Warmth>,
    within: Duration,
) -> Result<(), WorkspaceError> {
    // Only a copy of the state: the borrow `wait_for` returns must not live across awaits.
    let settled = tokio::time::timeout(within, warmth.wait_for(|w| *w != Warmth::Warming))
        .await
        .map(|r| r.map(|w| w.clone()));
    match settled {
        Ok(Ok(Warmth::Ready | Warmth::Failed)) => Ok(()),
        _ => Err(WorkspaceError::Unavailable(
            "Puddle is still downloading the image it reads a workspace's disk with; try again in a minute"
                .to_owned(),
        )),
    }
}

/// How a saved root-disk grow took effect.
struct Grown {
    how: &'static str,
    /// Why the restart that applies the grow failed; the grow itself is done.
    restart_failure: Option<String>,
}

impl Grown {
    fn done(how: &'static str) -> Self {
        Self {
            how,
            restart_failure: None,
        }
    }
}

/// The busy mark an accepted operation put on its workspace. [`HostWorkspaces::run`] takes it
/// and hands the mark to the task that does the work. Dropped before that (the API dropped the
/// request at its time limit or for a closed connection, or the request gave up with an error),
/// it takes the mark off, so no workspace stays busy for an operation that never ran. It leaves
/// the status as it is: an operation whose `enter` changes the status (start, stop) must reach
/// `run` with no await after `accept`.
#[must_use = "dropping the ticket takes the busy mark off again"]
struct Ticket<R: Runtime + Clone> {
    service: HostWorkspaces<R>,
    record: WorkspaceRecord,
    op: Operation,
    /// Cleared once `run` has spawned the work.
    armed: bool,
}

impl<R: Runtime + Clone> Ticket<R> {
    /// The workspace as the operation found it, with the busy mark on.
    fn record(&self) -> &WorkspaceRecord {
        &self.record
    }
}

impl<R: Runtime + Clone> Drop for Ticket<R> {
    fn drop(&mut self) {
        if self.armed {
            self.service.withdraw(&self.record.id, self.op);
        }
    }
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

/// After an agent session ends, how many times the sandboxes are read and how far apart.
const SESSION_END_READS: u32 = 5;
const SESSION_END_GAP: Duration = Duration::from_secs(2);

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
    workspace: WorkspaceId,
}

/// What the user is told when `workspace` has no agent link to sign in through.
fn no_link_message(workspace: &WorkspaceId, why: NoLink) -> String {
    match why {
        NoLink::NotRunning => format!("{workspace} is not running; start it first"),
        NoLink::NotConnected => format!(
            "{workspace} is running, but its editor has not connected to Puddle yet. Puddle keeps \
             trying; open the editor again in a moment"
        ),
    }
}

/// What the user is told when the guest gave no editor password. An image the editor cannot run
/// in never gets one, so that case says why instead of "yet".
fn editor_login_failure(err: &puddle_front::Unreachable) -> String {
    if err.0 == NEEDS_GLIBC {
        format!("{err}; use desktop VS Code over direct SSH or a glibc image")
    } else {
        format!("the editor has no password to sign in with yet: {err}")
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
            editor
                .password()
                .await
                .map_err(|err| LaunchError::new(editor_login_failure(&err)))
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
        mut retired: BTreeSet<WorkspaceId>,
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
            let mut record = WorkspaceRecord::new(id.clone(), name.clone(), stored.repositories);
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
        let service = Self {
            inner: Arc::new(Inner {
                runtime: parts.runtime,
                workspaces: parts.workspaces,
                lifecycle: parts.lifecycle,
                kit: parts.kit,
                events: parts.events,
                clock: parts.clock,
                settings: parts.settings,
                settings_lock: Arc::default(),
                store: parts.store,
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
                    retired,
                    closed: false,
                }),
                live: tokio::sync::Mutex::new(BTreeMap::new()),
                tasks: Mutex::new(JoinSet::new()),
                running: watch::channel(0).0,
                maintenance_image: watch::channel(Warmth::Cold).0,
                shell_init_changed_inside: Mutex::new(BTreeSet::new()),
                env_missed_while_booting: Mutex::new(BTreeSet::new()),
                listings: Mutex::new(BTreeMap::new()),
                disk_warned: Mutex::new(BTreeSet::new()),
            }),
        };
        service.note_names(&service.state());
        service.adopt_listed()?;
        if interrupted && let Err(e) = service.persist(&service.state()) {
            // Harmless: the entries still say "being created" or "being deleted", so the next
            // start settles them the same way, and every later save writes the list without them.
            tracing::warn!(error = %e, "the workspace list was not saved after dropping unfinished creates");
        }
        Ok(service)
    }

    /// On the first start of this build, records the owner as the user of every listed workspace
    /// that has none: they were made before every create recorded its user. Before anything serves
    /// them, or no user would reach them; later starts change nothing.
    fn adopt_listed(&self) -> Result<(), HostError> {
        let adopted = self
            .inner
            .store
            .adopt_unowned_workspaces(self.state().slots.keys())
            .map_err(|e| HostError::State {
                what: "the database",
                reason: format!("whose the listed workspaces are could not be recorded: {e}"),
            })?;
        if adopted > 0 {
            tracing::info!(
                adopted,
                "recorded the owner as the user of the workspaces made before"
            );
        }
        Ok(())
    }

    /// What the user calls workspace `id`, for text they read. Never the id: a real host draws
    /// random ones. A workspace the list no longer holds reads as its id, the one thing left.
    fn label(&self, id: &WorkspaceId) -> String {
        let name = self
            .state()
            .slots
            .get(id)
            .map(|slot| slot.record.name.clone());
        name_or_id(name.as_ref(), id)
    }

    fn state(&self) -> StateGuard<'_, R> {
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
    fn persist(&self, state: &State) -> Result<(), HostError> {
        let stored = state
            .slots
            .values()
            .map(|slot| Stored {
                id: slot.record.id.clone(),
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
        self.inner.book.save(stored, &state.retired)?;
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
    fn note_names(&self, state: &State) {
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
    fn write_noted_names(&self) {
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
            workspace: record.id.clone(),
            status: record.status,
        });
    }

    fn progress(&self, name: &WorkspaceId, step: WorkspaceStep, detail: Option<String>) {
        self.inner.events.emit(Event::WorkspaceProgress {
            workspace: name.clone(),
            step,
            detail,
            repository: None,
        });
    }

    /// The cloning step of one repository: the address as the detail, the directory as the
    /// repository it is about.
    fn progress_cloning(&self, name: &WorkspaceId, repository: &Repository) {
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

    fn accept(
        &self,
        id: &WorkspaceId,
        op: Operation,
        check: impl FnOnce(&WorkspaceRecord) -> Result<(), WorkspaceError>,
        enter: impl FnOnce(&mut WorkspaceRecord),
    ) -> Result<Ticket<R>, WorkspaceError> {
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
        Ok(self.ticket(slot.record.clone(), op))
    }

    /// The ticket of an operation that has just marked `record` busy.
    fn ticket(&self, record: WorkspaceRecord, op: Operation) -> Ticket<R> {
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
    fn withdraw(&self, id: &WorkspaceId, op: Operation) {
        let mut state = self.state();
        let Some(slot) = state.slots.get_mut(id) else {
            return;
        };
        if op == Operation::Creating {
            state.slots.remove(id);
            state.retired.insert(id.clone());
            if let Err(e) = self.persist(&state) {
                // Harmless: the saved entry says "being created", so the next start drops it.
                tracing::warn!(workspace = %id, error = %e, "the workspace list was not saved after a create was dropped");
            }
            return;
        }
        slot.record.busy = None;
        slot.deleting = false;
        slot.delete_note = None;
        let record = slot.record.clone();
        drop(state);
        tracing::info!(workspace = %id, %op, "a request was dropped before its work began");
        // The mark is off: say so, as the end of an operation does.
        self.status_event(&record);
    }

    fn tasks(&self) -> MutexGuard<'_, JoinSet<()>> {
        self.inner
            .tasks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Spawns `work` on a task the service owns: it runs to its end whatever becomes of the
    /// request that started it, unless shutdown ends it.
    fn spawn(&self, work: impl Future<Output = ()> + Send + 'static) {
        let mut tasks = self.tasks();
        while tasks.try_join_next().is_some() {}
        tasks.spawn(work);
    }

    /// Runs `job` on a task the service owns and returns what it returns. A caller dropped while
    /// it waits leaves the job to finish: a maintenance sandbox the job booted is stopped and
    /// removed, and its hold on the volume given back, only once the job has done so. Shutdown
    /// waits for the job like for an operation.
    async fn on_own_task<T: Send + 'static>(
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
    fn run<F>(&self, mut ticket: Ticket<R>, work: F)
    where
        F: Future<Output = Result<(), String>> + Send + 'static,
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

    fn status_of(&self, workspace: &WorkspaceId) -> Option<WorkspaceStatus> {
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
                    slot.record.status = WorkspaceStatus::Crashed;
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
    fn warn_about_full_disks(&self) {
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
    fn conclude(&self, id: &WorkspaceId, op: Operation, result: Result<(), String>) {
        let mut state = self.state();
        let Some(slot) = state.slots.get_mut(id) else {
            return;
        };
        slot.record.busy = None;
        slot.deleting = false;
        let delete_note = slot.delete_note.take();
        let name = slot.record.id.clone();
        let mut status_changed = false;
        // A failed create, and a delete either way, leave the list different from what is saved:
        // the failed create's entry goes, the failed delete's entry comes back unmarked, and the
        // finished delete's entry goes with its id retired.
        let save = matches!(
            (&result, op),
            (Err(_), Operation::Creating | Operation::Deleting) | (Ok(()), Operation::Deleting)
        );
        let mut result = result;
        let old_entry = slot.creating;
        match (&result, op) {
            (Err(_), Operation::Creating) => {
                // A failed create may leave rows and files keyed by its id: retired like a
                // deleted workspace's, so no later one meets them.
                state.slots.remove(id);
                state.retired.insert(id.clone());
            }
            (Ok(()), Operation::Deleting) => {
                state.slots.remove(id);
                state.retired.insert(id.clone());
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
            (
                Err(_),
                Operation::Reclaiming | Operation::ResettingRootDisk | Operation::Deleting,
            )
            | (Ok(()), Operation::Reclaiming | Operation::ResettingRootDisk)
            | (_, Operation::Cloning) => {}
            (Ok(()), Operation::GrowingRootDisk) => {
                // A grow that had to restart the workspace ends with it running again.
                if slot.record.status == WorkspaceStatus::Starting {
                    slot.record.status = WorkspaceStatus::Running;
                    status_changed = true;
                }
            }
            (Err(_), Operation::GrowingRootDisk) => match slot.record.status {
                // The restart failed in its start, or in its stop.
                WorkspaceStatus::Starting => {
                    slot.record.status = if std::mem::take(&mut slot.volume_missing) {
                        WorkspaceStatus::VolumeMissing
                    } else {
                        WorkspaceStatus::Crashed
                    };
                    status_changed = true;
                }
                WorkspaceStatus::Draining => {
                    slot.record.status = WorkspaceStatus::Running;
                    status_changed = true;
                    self.inner.reports.resume(&name);
                }
                _ => {}
            },
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
        self.audit_operation(&name, op, &result, delete_note);
        match result {
            Ok(()) => self.progress(&name, WorkspaceStep::Done, None),
            Err(reason) => {
                tracing::warn!(workspace = %id, %op, %reason, "workspace operation failed");
                self.progress(&name, WorkspaceStep::Failed, Some(reason));
            }
        }
    }

    /// Writes what an operation came to into the audit log: a create or a delete either way, a
    /// start or a stop only when it failed (a start that worked shows in the workspace's state).
    fn audit_operation(
        &self,
        name: &WorkspaceId,
        op: Operation,
        result: &Result<(), String>,
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
            Err(reason) => (WorkspaceOpOutcome::Failed, Some(reason.clone())),
        };
        self.record_op(kind, name, outcome, detail);
    }

    fn record_op(
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
    fn default_memory(&self) -> Result<MemoryMib, String> {
        let global = crate::settings_read::global(self.inner.settings.as_ref())?;
        Ok(resolve(&global, None::<&WorkspaceSettings>).memory.value)
    }

    /// The vCPUs for `name`: its override over the global default over one. The default when the
    /// settings cannot be read (a workspace is not worth a failed start over a CPU count).
    fn cpus(&self, name: &WorkspaceId) -> CpuCount {
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
    async fn maintenance_ready(&self) -> Result<(), WorkspaceError> {
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
    async fn maintenance_job<T: Send + 'static>(
        &self,
        job: impl Future<Output = T> + Send + 'static,
    ) -> Result<T, WorkspaceError> {
        self.maintenance_ready().await?;
        self.on_own_task(job).await
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
        let (id, name) = (&record.id, &record.id);
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
        if let Err(reason) = self.publish_shell_init(id) {
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
                    .with_root_disk(root_disk_size(record.root_disk_grown_mib))
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
            &record.id,
            image,
            record.start_folder.as_ref(),
            record.session_user,
        )
        .await
    }

    /// Makes this start's CA for `name`, registers what it decrypts and its secrets' stand-ins and
    /// plans the guest's boot with the CA in its trust, the authors of the workspace's identities
    /// and its environment. A start that fails after this must call `Injection::end`.
    async fn plan_guest(
        &self,
        id: &WorkspaceId,
        name: &WorkspaceId,
        image: ImageConfig,
        start_folder: Option<&StartFolder>,
        session_user: SessionUser,
    ) -> Result<(puddle_boot::BootPlan, GuestEnv, GuestState), String> {
        let inner = &self.inner;
        let capture = crate::logins::capture_enabled(inner.settings.as_ref(), name);
        let editor = inner
            .kit
            .editor(id, self.reconnection_grace(name), self.editor_kind())?
            .map(|server| server.with_user(session_user));
        let began = inner.injection.begin(name, capture).await?;
        let wanted = authors(&began.git);
        self.report_git_settings(name, &began.git, &wanted);
        let root = Layout::new(id).map_err(|e| e.to_string())?.mount().clone();
        // Only a running guest can list the checkouts, so the multi-root file the last attach
        // wrote stays as it is and the editor server opens it at this start; the next attach
        // writes it again. A start folder replaces it.
        let editor_file = start_folder
            .is_none()
            .then(|| crate::editor_file::workspace_file_path(&root, id))
            .flatten()
            .map(GuestFile::kept);
        let state = GuestState {
            editor,
            image,
            ca: began.certificate,
            authors: wanted,
            ssh: began.ssh,
            env: began.env,
            dev_cert: inner.kit.dev_cert().await,
            start_folder: start_folder.cloned(),
            root,
            editor_file,
            session_user,
        };
        match inner.kit.plan(
            &state.image,
            &GuestInputs {
                ca: &state.ca,
                authors: &state.authors,
                ssh: state.ssh.as_ref(),
                env: &state.env,
                editor: state.editor.as_ref(),
                dev_cert: state.dev_cert.as_ref(),
                root: &state.root,
                start_folder: state.start_folder.as_ref(),
                editor_file: state.editor_file.as_ref(),
                session_user: state.session_user,
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
    pub(crate) async fn git_changed(&self, name: &WorkspaceId) {
        if self.decrypt_changed(name).is_some() {
            self.rewrite_authors(name).await;
        }
    }

    /// Recomputes what the running `name` decrypts from the database: the next connection sees
    /// the new set. The settings of a workspace that does not run, or that cannot be read, change
    /// nothing here.
    pub(crate) fn decrypt_changed(&self, name: &WorkspaceId) -> Option<puddle_store::WorkspaceGit> {
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
                        format!(
                            "Puddle could not update what {name} decrypts",
                            name = self.label(name)
                        ),
                        format!(
                            "{reason}. {name} keeps the credentials it had; the next change to its \
                             identities tries again.",
                            name = self.label(name)
                        ),
                    )),
                );
                None
            }
        }
    }

    /// Tells the user about the credentials of `git` that Puddle never uses.
    fn report_credentials(&self, name: &WorkspaceId, git: &puddle_store::WorkspaceGit) {
        let hosts = never_decrypted(git);
        GitProblem::Credentials.settle(
            &self.inner.problems,
            name,
            credentials_problem(&self.label(name), &hosts),
        );
    }

    /// Tells the user about what the Git settings of `name` ask for that Puddle cannot carry out.
    fn report_git_settings(
        &self,
        name: &WorkspaceId,
        git: &puddle_store::WorkspaceGit,
        wanted: &Authors,
    ) {
        self.report_credentials(name, git);
        GitProblem::LeftOut.settle(
            &self.inner.problems,
            name,
            left_out_problem(&self.label(name), wanted),
        );
    }

    /// After a boot: Kubernetes tools in the guest are a problem on the workspace, since its
    /// kernel cannot route their Services. A stop ends it, so a start that finds none has none.
    fn settle_kubernetes(&self, name: &WorkspaceId, report: &puddle_boot::BootReport) {
        if let Some(problem) =
            kubernetes_problem(name, &self.label(name), &report.kubernetes_tools())
        {
            self.inner.problems.raise(problem);
        }
    }

    /// After the hook ran (a boot or a rewrite of the plan in the running guest): each file of the
    /// user's that Puddle merges settings into and could not parse is a notice on the workspace.
    /// The file is left as it is, and the run goes on. A file the hook no longer leaves ends its
    /// notice, so a repair is noticed at the next run and a stop ends them all.
    fn settle_merge_notices(&self, name: &WorkspaceId, report: &puddle_boot::BootReport) {
        settle_merge_notices(
            &self.inner.problems,
            name,
            &self.label(name),
            &report.left_files(),
        );
    }

    /// After the hook ran with `wanted` authors: a guest whose git is too old for the author rules
    /// is a problem on the workspace.
    fn settle_git_version(
        &self,
        name: &WorkspaceId,
        report: &puddle_boot::BootReport,
        wanted: &Authors,
    ) {
        GitProblem::OldGit.settle(
            &self.inner.problems,
            name,
            old_git_problem(&self.label(name), report.git_version(), wanted),
        );
    }

    /// Runs the boot plan again in the running guest when its authors differ from what the guest
    /// holds (the hook is idempotent: only the author files change).
    ///
    /// The settings are read once the guest is ours alone: rewrites for one workspace start in
    /// any order, and a copy taken when the change arrived could be applied after a newer one,
    /// leaving the old authors and a problem the newer settings already ended.
    pub(crate) async fn rewrite_authors(&self, name: &WorkspaceId) {
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
                self.raise_author_sync(name, &reason);
                return;
            }
        };
        let wanted = authors(&git);
        self.report_git_settings(name, &git, &wanted);
        // The keys stay the start's; the names they are listed under follow the identities.
        let listing = guest
            .ssh
            .as_ref()
            .map(|held| SshListing::for_git(Arc::clone(&held.keys), &git));
        if guest.authors == wanted
            && guest.ssh.as_ref().map(|s| &s.hosts) == listing.as_ref().map(|s| &s.hosts)
        {
            return;
        }
        let replanned = GuestState {
            authors: wanted.clone(),
            ssh: listing.clone(),
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
                    state.ssh = listing;
                }
            }
            Err(reason) => self.raise_author_sync(name, &reason),
        }
    }

    /// Tells the user the commit authors in `name` could not be updated and why.
    fn raise_author_sync(&self, name: &WorkspaceId, reason: &str) {
        GitProblem::AuthorSync.settle(
            &self.inner.problems,
            name,
            Some((
                format!(
                    "Puddle could not update the commit authors in {name}",
                    name = self.label(name)
                ),
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
        name: &WorkspaceId,
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
                        format!(
                            "Puddle could not look for {name}'s start folder {folder}",
                            name = self.label(name)
                        ),
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
                    format!(
                        "{name}'s start folder {folder} does not exist",
                        name = self.label(name)
                    ),
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
        let name = &record.id;
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
                        format!(
                            "Puddle could not move {name}'s terminals to the new start folder",
                            name = self.label(name)
                        ),
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
        let name = &record.id;
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
                    editor_file_title(record),
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
        let left = report.left_files();
        match left
            .iter()
            .find(|(path, _)| path.ends_with(".code-workspace"))
        {
            Some((path, why)) => inner.problems.raise(
                Problem::new(
                    key,
                    editor_file_title(record),
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
        name: &WorkspaceId,
        gated: &GatedSandbox<R::Sandbox>,
        guest: &GuestState,
    ) -> Result<(), String> {
        self.run_plan_report(name, gated, guest).await.map(|_| ())
    }

    /// As [`Self::run_plan`], with what the hook said.
    async fn run_plan_report(
        &self,
        name: &WorkspaceId,
        gated: &GatedSandbox<R::Sandbox>,
        guest: &GuestState,
    ) -> Result<puddle_boot::BootReport, String> {
        let kit = &self.inner.kit;
        let inputs = GuestInputs {
            ca: &guest.ca,
            authors: &guest.authors,
            ssh: guest.ssh.as_ref(),
            env: &guest.env,
            editor: guest.editor.as_ref(),
            dev_cert: guest.dev_cert.as_ref(),
            root: &guest.root,
            start_folder: guest.start_folder.as_ref(),
            editor_file: guest.editor_file.as_ref(),
            session_user: guest.session_user,
        };
        let (plan, _) = kit.plan(&guest.image, &inputs)?;
        let report = kit
            .hook()
            .run(gated.ungated(), &plan)
            .await
            .map_err(|failure| failure.to_string())?;
        self.announce_loader_replaced(name, &report);
        self.settle_merge_notices(name, &report);
        Ok(report)
    }

    fn announce_loader_replaced(&self, name: &WorkspaceId, report: &puddle_boot::BootReport) {
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
    pub(crate) async fn environment_changed(&self, name: &WorkspaceId) {
        if let Some(env) = self.inner.injection.environment_changed(name).await {
            self.rewrite_env(name, env).await;
        }
    }

    fn note_env_missed(&self, name: &WorkspaceId) {
        self.inner
            .env_missed_while_booting
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(name.clone());
    }

    /// Writes `env` into the running guest's login-shell file by running the boot plan again (the
    /// hook is idempotent: only the files that differ change).
    async fn rewrite_env(&self, name: &WorkspaceId, env: GuestEnv) {
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
                         until it restarts: {reason}",
                        name = self.label(name)
                    ),
                });
            }
        }
    }

    /// Writes the saved shell init of `id` to the folder its guest sees, for a start. A folder that
    /// held something else is put right and remembered, so `settle` tells the user.
    fn publish_shell_init(&self, id: &WorkspaceId) -> Result<(), String> {
        let published = self.inner.kit.shell_init().publish(id)?;
        if published == crate::shell_init::Published::Restored {
            self.inner
                .shell_init_changed_inside
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(id.clone());
        }
        Ok(())
    }

    /// The user saved or removed `name`'s shell init: the folder the running guest sees follows
    /// at once. A workspace that is gone is left alone.
    pub(crate) fn shell_init_changed(&self, name: &WorkspaceId) {
        let id = self
            .state()
            .slots
            .iter()
            .find(|(_, slot)| &slot.record.id == name && !slot.deleting)
            .map(|(id, _)| id.clone());
        let Some(id) = id else { return };
        let key = problem_key("shell-init-copy", name);
        match self.inner.kit.shell_init().refresh(&id) {
            Ok(_) => self.inner.problems.clear(&key),
            Err(reason) => {
                tracing::warn!(workspace = %name, %reason, "the workspace's shell init is not updated");
                self.inner.problems.raise(
                    Problem::new(
                        key,
                        format!(
                            "Puddle could not update {name}'s shell init",
                            name = self.label(name)
                        ),
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
    fn settle_entrypoint(&self, name: &WorkspaceId, report: &puddle_boot::BootReport) {
        if let Some(failure) = report.entrypoint_failure() {
            tracing::warn!(workspace = %name, %failure, "the image's entrypoint exited at once");
            self.inner
                .problems
                .raise(entrypoint_problem(name, &self.label(name), failure));
        }
    }

    /// After a boot: an image with no `sudo` for a session user that is not root becomes a problem
    /// on the workspace (the start goes on: everything but `sudo` works); a boot that finds `sudo`,
    /// or a root session user, ends the problem of an earlier boot.
    fn settle_no_sudo(&self, name: &WorkspaceId, report: &puddle_boot::BootReport) {
        if report.session_user_has_no_sudo() {
            tracing::warn!(workspace = %name, "the image has no sudo for the session user");
            self.inner
                .problems
                .raise(no_sudo_problem(name, &self.label(name)));
        } else {
            clear_no_sudo(&self.inner.problems, name);
        }
    }

    /// After a boot: tell the user when something inside the workspace had changed its shell init
    /// (the folder, or the file that loads it), and check that the saved script still parses and
    /// prints nothing. A script that fails the check does not stop the start: it is a problem on
    /// the workspace, because a start that refuses over a user's own script would lock them out of
    /// the way to fix it.
    async fn settle_shell_init(&self, name: &WorkspaceId, gated: &GatedSandbox<R::Sandbox>) {
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
        match self.run_shell_init_check(name, gated, &script).await {
            Ok(None) => inner.problems.clear(&key),
            Ok(Some(why)) => inner.problems.raise(
                Problem::new(
                    key,
                    format!("{name}'s shell init needs fixing", name = self.label(name)),
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
                        format!(
                            "Puddle could not check {name}'s shell init",
                            name = self.label(name)
                        ),
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
        name: &WorkspaceId,
        gated: &GatedSandbox<R::Sandbox>,
        script: &str,
    ) -> Result<Option<String>, String> {
        let user = self.booted_user(name).await;
        match gated
            .ungated()
            .exec(crate::shell_init::check_request(script, user))
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
        name: &WorkspaceId,
        locks: Result<puddle_workspace::LockReport, puddle_workspace::WorkspaceError>,
    ) {
        let key = problem_key("git-locks", name);
        if let Ok(report) = &locks {
            self.report_git_repairs(name, report);
        }
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
                format!(
                    "Puddle could not clear the stale git lock files in {name}",
                    name = self.label(name)
                ),
                format!(
                    "{why}. The workspace started, but git may say \"Another git process seems to \
                     be running\" until the .lock files in the repository's .git folder are \
                     deleted by hand."
                ),
            )
            .about(name.clone()),
        );
    }

    /// Audits each git file the boot rebuilt, and tells the user what was repaired and what was
    /// found damaged and left alone; a boot that finds neither withdraws the notice.
    fn report_git_repairs(&self, name: &WorkspaceId, report: &puddle_workspace::LockReport) {
        for repair in &report.repaired {
            self.inner.store.note_workspace_op(&NewWorkspaceOp {
                op: WorkspaceOpKind::GitRepaired,
                workspace: Some(name.clone()),
                user: None,
                outcome: WorkspaceOpOutcome::Succeeded,
                actor: Actor::System,
                detail: Some(repair.clone()),
            });
        }
        let key = problem_key("git-repair", name);
        if report.repaired.is_empty() && report.damaged.is_empty() {
            self.inner.problems.clear(&key);
            return;
        }
        let mut parts = Vec::new();
        if !report.repaired.is_empty() {
            parts.push(format!(
                "The last shutdown left git files unreadable. Puddle repaired them: {}. Your files \
                 are unchanged; anything you had staged with git add and not committed is no longer \
                 staged, so run git status and add it again.",
                report.repaired.join("; ")
            ));
        }
        if !report.damaged.is_empty() {
            parts.push(format!(
                "Puddle found damage it could not repair safely and left it as it is: {}. Git will \
                 not work in that repository until it is fixed by hand.",
                report.damaged.join("; ")
            ));
        }
        let detail = parts.join(" ");
        let title = if report.damaged.is_empty() {
            format!(
                "Puddle repaired git files in {name} after the last shutdown",
                name = self.label(name)
            )
        } else {
            format!(
                "Puddle found damaged git files in {name} that it could not repair",
                name = self.label(name)
            )
        };
        self.inner
            .problems
            .raise(Problem::new(key, title, detail).about(name.clone()));
    }

    /// After a boot: Puddle's directory and stale locks on the volume, the notice and check for the
    /// shell init, the SSH endpoint, and the handle the lifecycle stops at exit.
    async fn settle(&self, record: &WorkspaceRecord) -> Result<(), String> {
        let inner = &self.inner;
        let (id, name) = (&record.id, &record.id);
        self.progress(name, WorkspaceStep::Syncing, None);
        let gated = {
            let live = inner.live.lock().await;
            live.get(name)
                .and_then(|l| l.gated.clone())
                .ok_or_else(|| format!("{name} is not running"))?
        };
        let locks = inner
            .workspaces
            .after_boot(gated.ungated(), id, self.booted_user(name).await)
            .await
            .map_err(|e| e.to_string())?;
        self.report_locks(name, locks);
        self.settle_shell_init(name, &gated).await;
        self.settle_entrypoint(name, gated.boot_report());
        self.settle_no_sudo(name, gated.boot_report());
        self.settle_kubernetes(name, gated.boot_report());
        self.settle_merge_notices(name, gated.boot_report());
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
    pub fn direct_ssh_allowed(&self, name: &WorkspaceId) -> bool {
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
        let name = &record.id;
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
        let user = guest.map_or(SessionUser::Root, |g| g.session_user);
        let open = match guest {
            Some(guest) => Self::open_target(record, guest),
            None => return Err(format!("{name} is not running; start it first")),
        };
        let key = problem_key("ssh", name);
        self.inner
            .ssh
            .publish(name, &endpoint, user)
            .map_err(|reason| {
                self.ssh_not_usable(name, &reason);
                format!("direct SSH is on but the ssh config could not be written: {reason}")
            })?;
        self.inner.problems.clear(&key);
        Ok(DesktopTarget {
            ssh_host: ssh_host(name),
            path: open.path,
            is_workspace_file: open.is_workspace_file,
        })
    }

    /// What an editor opens in the running `guest`: the multi-root file when the editor opens one,
    /// else the start folder or the volume root. The desktop and the browser attach share it.
    fn open_target(record: &WorkspaceRecord, guest: &GuestState) -> OpenTarget {
        match &guest.editor_file {
            Some(file) => OpenTarget {
                path: file.path().as_str().to_owned(),
                is_workspace_file: true,
            },
            None => OpenTarget {
                path: crate::editor_file::start_path(&guest.root, record.start_folder.as_ref())
                    .as_str()
                    .to_owned(),
                is_workspace_file: false,
            },
        }
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
    fn reconnection_grace(&self, name: &WorkspaceId) -> u32 {
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
        let name = &record.id;
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
        // Read under the live lock, released before the launcher is asked.
        let open = {
            let live = self.inner.live.lock().await;
            live.get(name)
                .and_then(|e| e.guest.as_ref())
                .map(|guest| Self::open_target(record, guest))
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
            open,
        };
        match self.inner.launcher.open_browser(record, &target).await {
            Ok(()) => Attached::opened(),
            Err(err) => Attached::not_opened(err.to_string()),
        }
    }

    /// What `name`'s settings say about its window, or why they cannot be read.
    fn window_settings(&self, name: &WorkspaceId) -> Result<Effective, String> {
        let settings = self.inner.settings.as_ref();
        let global = crate::settings_read::global(settings)?;
        let own = crate::settings_read::workspace(settings, name)?;
        Ok(resolve(&global, Some(&own)))
    }

    fn direct_ssh_setting(&self, name: &WorkspaceId) -> Result<bool, String> {
        let settings = self.inner.settings.as_ref();
        let global = crate::settings_read::global(settings)?;
        let own = crate::settings_read::workspace(settings, name)?;
        Ok(resolve(&global, Some(&own)).direct_ssh.value)
    }

    /// Opens the SSH endpoint of a running sandbox and puts its entry in Puddle's ssh config. A
    /// failure is reported to the user ([`Event::WorkspaceSshFailed`] and a problem that lasts
    /// until it works): the workspace runs without direct SSH.
    async fn open_ssh(&self, name: &WorkspaceId, gated: Arc<GatedSandbox<R::Sandbox>>) {
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
                    let user = entry
                        .guest
                        .as_ref()
                        .map_or(SessionUser::Root, |g| g.session_user);
                    // The entry is written while the lock is held, so a switch turned off
                    // meanwhile (which takes the endpoint under the same lock) removes it
                    // after, never before.
                    self.publish_entry(name, &endpoint, user);
                    stale
                }
                None => Some(ssh),
            }
        };
        if let Some(leftover) = leftover {
            leftover.close().await;
        }
    }

    /// Puts `name`'s entry in Puddle's ssh config, naming `endpoint` and logging in as `user`. The
    /// caller has checked [`Self::direct_ssh_allowed`]. A failure is a problem on the workspace:
    /// its endpoint is up, but nothing can name it.
    fn publish_entry(&self, name: &WorkspaceId, endpoint: &std::path::Path, user: SessionUser) {
        let key = problem_key("ssh", name);
        match self.inner.ssh.publish(name, endpoint, user) {
            Ok(()) => self.inner.problems.clear(&key),
            Err(reason) => self.ssh_not_usable(name, &reason),
        }
    }

    fn endpoint_failed(&self, name: &WorkspaceId, error: &dyn std::fmt::Display) {
        report_no_endpoint(self.inner.events.as_ref(), name, error);
        self.ssh_not_usable(name, &format!("it has no SSH endpoint ({error})"));
    }

    /// Raises the problem that direct SSH is on for `name` and cannot be used.
    fn ssh_not_usable(&self, name: &WorkspaceId, reason: &str) {
        tracing::warn!(workspace = %name, %reason, "direct SSH is on but not usable");
        self.inner.problems.raise(
            Problem::new(
                problem_key("ssh", name),
                format!(
                    "Direct SSH does not work in {name}",
                    name = self.label(name)
                ),
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
    async fn close_ssh(&self, name: &WorkspaceId, ssh: SshEndpoint) {
        let key = problem_key("ssh", name);
        match self.inner.ssh.withdraw(name) {
            Ok(()) => self.inner.problems.clear(&key),
            Err(reason) => {
                tracing::warn!(workspace = %name, %reason, "the ssh config still names a closed endpoint");
                self.inner.problems.raise(
                    Problem::new(
                        key,
                        format!(
                            "Puddle's ssh config still names {name}",
                            name = self.label(name)
                        ),
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
        type Running<S> = (WorkspaceId, Option<Arc<GatedSandbox<S>>>, bool);
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
    async fn quiesce(&self, name: &WorkspaceId, drop_route: bool) {
        self.inner.lifecycle.release(&name.sandbox_name());
        let reason = self.terminal_gone(name);
        self.inner.terminals.end(name, reason);
        // Its forwards close; the pinned ones come back at the next start.
        self.inner.ports.stopped(name).await;
        self.inner.flavours.stopped(name);
        // The CA belongs to one start: a stopped or failed sandbox decrypts nothing.
        self.inner.injection.end(name);
        self.inner.problems.clear(&problem_key("ssh", name));
        GitProblem::clear_all(&self.inner.problems, name);
        self.inner.problems.clear(&kubernetes_key(name));
        self.inner.problems.clear_prefix(&merge_notice_prefix(name));
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
        let name = &record.id;
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

    async fn create_work(
        &self,
        record: WorkspaceRecord,
        imports: Vec<Import>,
    ) -> Result<(), String> {
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
        if let Err(e) = self.mark_created(&record.id) {
            let reason = format!("{e}; the new workspace was removed again; {SAVE_HINT}");
            return Err(self.undo_boot(&record, true, created_volume, reason).await);
        }
        self.refresh_checkouts_later(&record.id);
        Ok(())
    }

    /// Clones the checkouts on this computer of a new workspace onto its volume, before its
    /// sandbox boots (see [`Workspaces::import`]). Returns the directories they went to (cloned or
    /// refused: neither is cloned again from its address) and whether this made the volume. A
    /// checkout git refuses fails only its repository, as a clone does.
    async fn import_all(
        &self,
        record: &WorkspaceRecord,
        imports: &[Import],
    ) -> Result<(BTreeSet<CheckoutDir>, bool), String> {
        if imports.is_empty() {
            return Ok((BTreeSet::new(), false));
        }
        let image = ImageRef::new(&record.image).map_err(|e| e.to_string())?;
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
            .await
            .map_err(|e| e.to_string())?;
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
    async fn remove_new_volume(&self, record: &WorkspaceRecord, reason: String) -> String {
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
    async fn clone_all(
        &self,
        record: &WorkspaceRecord,
        sandbox: &R::Sandbox,
        imported: &BTreeSet<CheckoutDir>,
    ) -> Result<(), String> {
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
                Err(other) => return Err(other.to_string()),
            }
        }
        if !any_cloned {
            return Ok(());
        }
        workspaces
            .sync(sandbox, &record.id)
            .await
            .map_err(|e| e.to_string())
    }

    /// The user the running guest of `name` started with, which its clones and checks run as
    /// until the next start whatever the setting says now; root when it does not run.
    async fn booted_user(&self, name: &WorkspaceId) -> SessionUser {
        self.inner
            .live
            .lock()
            .await
            .get(name)
            .and_then(|l| l.guest.as_ref())
            .map_or(SessionUser::Root, |g| g.session_user)
    }

    async fn gated(&self, name: &WorkspaceId) -> Result<Arc<GatedSandbox<R::Sandbox>>, String> {
        self.inner
            .live
            .lock()
            .await
            .get(name)
            .and_then(|l| l.gated.clone())
            .ok_or_else(|| format!("{name} is not running"))
    }

    /// Why a terminal's connection that ended without an exit status did: the workspace crashed,
    /// or it stopped (or went away).
    fn terminal_gone(&self, name: &WorkspaceId) -> EndReason {
        crate::terminals::reason_for(self.state().slots.get(name).map(|s| s.record.status))
    }

    /// The running guest's sandbox and the user its sessions run as, once the workspace is
    /// running; a workspace that is starting is waited for up to [`TERMINAL_BOOT_WAIT`].
    async fn terminal_target(
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
    async fn reset_root_disk_work(&self, name: &WorkspaceId) -> Result<(), String> {
        let sandbox = name.sandbox_name();
        let exists = self
            .inner
            .runtime
            .list()
            .await
            .map_err(|e| e.to_string())?
            .iter()
            .any(|s| s.name == sandbox.as_str());
        if exists {
            self.inner
                .runtime
                .remove(&sandbox)
                .await
                .map_err(|e| e.to_string())?;
            self.inner.workspaces.sandbox_removed(&sandbox);
        }
        self.inner.problems.clear(&problem_key("root-copy", name));
        // A route left from an earlier boot belongs to a sandbox that no longer exists.
        self.quiesce(name, true).await;
        Ok(())
    }

    /// Sets the workspace's status and tells the windows.
    fn set_status(&self, id: &WorkspaceId, status: WorkspaceStatus) {
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
    async fn grow_root_disk_work(
        &self,
        record: WorkspaceRecord,
        from_mib: u64,
        size: DiskSize,
        room: Room,
    ) -> Result<(), String> {
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
    async fn grow_root_disk_steps(
        &self,
        record: &WorkspaceRecord,
        size: DiskSize,
    ) -> Result<Grown, String> {
        let inner = &self.inner;
        let name = &record.id;
        self.progress(name, WorkspaceStep::GrowingRootDisk, None);
        let sandbox = name.sandbox_name();
        let exists = inner
            .runtime
            .list()
            .await
            .map_err(|e| e.to_string())?
            .iter()
            .any(|s| s.name == sandbox.as_str());
        let growth = if exists {
            Some(
                inner
                    .runtime
                    .grow_root_disk(&sandbox, size)
                    .await
                    .map_err(|e| e.to_string())?,
            )
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
                .map_err(|e| format!("{e}; {SAVE_HINT}"))?;
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

    async fn start_work(&self, record: WorkspaceRecord) -> Result<(), String> {
        let inner = &self.inner;
        let name = &record.id;
        self.progress(name, WorkspaceStep::Starting, None);
        inner.problems.clear(&problem_key("forced-stop", name));
        inner.problems.clear(&problem_key("start-refused", name));
        inner.reports.begin(name, &record.name);
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
        self.refresh_checkouts_later(&record.id);
        Ok(())
    }

    /// Starts the stopped sandbox again. `Ok(false)` when it did not: the workspace's variables
    /// changed since the sandbox was made and a sandbox keeps the environment it was made with, so
    /// the caller makes a new one on the same volume (the volume holds the work, not the sandbox).
    async fn restart_in_place(&self, record: &WorkspaceRecord) -> Result<bool, String> {
        let inner = &self.inner;
        let name = &record.id;
        let image = ImageRef::new(&record.image).map_err(|e| e.to_string())?;
        let config = self.image_config(&image).await?;
        self.publish_shell_init(&record.id)?;
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
        let name = &record.id;
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
            .map_err(|e| {
                if let BootError::Compute(ComputeError::StartRefused { cause, .. }) = &e {
                    self.raise_start_refused(name, *cause);
                }
                boot_message(&e)
            })
    }

    /// Raises the problem that the runtime refused to start `name` and why.
    fn raise_start_refused(&self, name: &WorkspaceId, cause: StartRefusal) {
        self.inner.problems.raise(
            Problem::new(
                problem_key("start-refused", name),
                format!("{name} was not started", name = self.label(name)),
                format!(
                    "The runtime refused to start it, and nothing was attached: {cause}. {}",
                    cause.way_out()
                ),
            )
            .about(name.clone()),
        );
    }

    async fn stop_work(&self, record: WorkspaceRecord) -> Result<(), String> {
        let name = &record.id;
        // The terminals end first, with the stop as their reason, while the guest can still answer.
        self.inner.terminals.end(name, EndReason::Stopped);
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
                        format!(
                            "{name}'s disk was not trimmed when it stopped",
                            name = self.label(name)
                        ),
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
                    format!(
                        "{name} did not shut down in time and was ended by force",
                        name = self.label(name)
                    ),
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

    /// Deletes the workspace's settings document. A failure is a notice (the file stays). A new
    /// workspace has a new id, so it never inherits a stuck file.
    /// Under the settings routes' lock, so a settings change that read the file before this
    /// removal cannot write it back after.
    async fn forget_settings_file(&self, id: &WorkspaceId, name: &str) {
        let inner = &self.inner;
        let key = problem_key("settings", id);
        let _lock = inner.settings_lock.lock().await;
        match inner.settings.delete_workspace(id) {
            Ok(()) => inner.problems.clear(&key),
            Err(e) => inner.problems.raise(Problem::new(
                key,
                format!("{name}'s settings were not deleted"),
                format!(
                    "{e}. No later workspace gets the id {id}, so none inherits them; delete \
                     the file by hand to tidy up."
                ),
            )),
        }
    }

    /// Deletes what the browser editor kept under `name`: the front's half of Microsoft's secret
    /// key and the windows' browser data. Returns what could not be deleted, one reason each.
    async fn clear_browser_state(&self, name: &WorkspaceId) -> Vec<String> {
        let inner = &self.inner;
        let mut failed = Vec::new();
        // The windows first: one still open could mint the key again after it was forgotten.
        if let Err(e) = inner.launcher.forget_browser(name).await {
            failed.push(e.to_string());
        }
        if let Err(e) = inner.front_halves.forget(name) {
            failed.push(format!("the editor's secret key could not be deleted: {e}"));
        }
        failed
    }

    /// The names the runtime has for volumes and sandboxes, which a new id must not meet.
    async fn runtime_names(&self) -> Result<RuntimeNames, WorkspaceError> {
        let unavailable = |e: ComputeError| {
            WorkspaceError::Unavailable(format!(
                "Puddle cannot list the runtime's disks and sandboxes, so it cannot pick an id \
                 no earlier workspace used: {e}"
            ))
        };
        let volumes = self
            .inner
            .runtime
            .list_volumes()
            .await
            .map_err(unavailable)?;
        let sandboxes = self.inner.runtime.list().await.map_err(unavailable)?;
        Ok(RuntimeNames {
            volumes: volumes.into_iter().map(|v| v.name).collect(),
            sandboxes: sandboxes.into_iter().map(|s| s.name).collect(),
        })
    }

    /// Whether `id` is in use or was ever used: listed or retired, a volume `ws-<id>`, a
    /// sandbox `<id>` or `m--<id>`, or anything Puddle's data folder keys by it (browser data,
    /// the front's key half, a settings file, a login marker). A settings file that cannot be
    /// read counts as there.
    fn id_taken(&self, state: &State, runtime: &RuntimeNames, id: &WorkspaceId) -> bool {
        let inner = &self.inner;
        state.slots.contains_key(id)
            || state.retired.contains(id)
            || runtime.volumes.contains(id.volume_name().as_str())
            || runtime.sandboxes.contains(id.as_str())
            || runtime
                .sandboxes
                .contains(&format!("{}{id}", puddle_workspace::MAINTENANCE_PREFIX))
            || inner.launcher.has_browser_data(id)
            || inner.front_halves.has(id)
            || !matches!(inner.settings.load_workspace(id), Ok(None))
            || puddle_logins::Marker::for_workspace(&inner.login_markers, id).exists()
    }

    /// The editor's origin is `<id>.localhost`, so a workspace with this id would reach the
    /// browser data an earlier one left (a delete cut short, or made where no window could clean
    /// it). No window can exist for a workspace that is still being created: clear it once the id is reserved.
    async fn clear_left_over_browser_state(
        &self,
        id: &WorkspaceId,
        name: &DisplayName,
    ) -> Result<(), WorkspaceError> {
        let failed = self.clear_browser_state(id).await;
        if failed.is_empty() {
            self.inner.problems.clear(&problem_key("browser-data", id));
            return Ok(());
        }
        // The create is refused; the id it reserved is retired like a failed create's.
        let mut state = self.state();
        state.slots.remove(id);
        state.retired.insert(id.clone());
        // Saved now: no entry carries this id, so a crash before the next save would forget it.
        if let Err(e) = self.persist(&state) {
            tracing::warn!(workspace = %id, error = %e, "the retired id was not saved after a refused create");
        }
        drop(state);
        Err(WorkspaceError::Conflict(format!(
            "browser data left under the id {id} could not be removed ({}), and the new \
             workspace would inherit it; {name} was not created. Close its editor windows and \
             try again",
            failed.join("; ")
        )))
    }

    /// Deletes what the browser editor kept for a deleted workspace. A failure is a notice: the
    /// data stays under an id no later workspace gets.
    async fn forget_browser_state(&self, id: &WorkspaceId, name: &str) {
        let inner = &self.inner;
        let key = problem_key("browser-data", id);
        let failed = self.clear_browser_state(id).await;
        if failed.is_empty() {
            inner.problems.clear(&key);
        } else {
            inner.problems.raise(Problem::new(
                key,
                format!("{name}'s browser editor data was not deleted"),
                format!(
                    "{}. No later workspace gets the id {id}, so none reaches it; to tidy up, \
                     close the editor windows and delete its folders under webview and front in \
                     Puddle's data folder.",
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
        self.progress(&record.id, WorkspaceStep::Checking, None);
        inner.launcher.close_browser(&record.id).await;
        self.progress(&record.id, WorkspaceStep::Removing, None);
        inner
            .workspaces
            .delete(&inner.runtime, &record.id, &report.confirm())
            .await
            .map_err(|e| e.to_string())?;
        self.quiesce(&record.id, true).await;
        self.inner.reports.forget(&record.id);
        inner.kit.shell_init().forget(&record.id);
        inner
            .problems
            .clear_prefix(&format!("{}:", problem_key("clone", &record.id)));
        self.forget_leftovers(&record.id, record.name.as_str())
            .await;
        Ok(())
    }

    /// Removes what a deleted workspace left under its id outside the runtime: its rules, its
    /// identities, its environment and the values of its own secrets, its settings file and the
    /// browser editor's data. Every step finds nothing the second time, so the sweep at start
    /// can run it again for any retired id; a step that fails is a notice and the next start
    /// tries again.
    async fn forget_leftovers(&self, id: &WorkspaceId, label: &str) {
        self.inner.injection.forget(id).await;
        self.forget_settings_file(id, label).await;
        self.forget_browser_state(id, label).await;
    }

    /// Finishes what a delete that did not end left behind, for every retired id: a sandbox
    /// still named after it, and the leftovers [`Self::forget_leftovers`] removes. Run once in
    /// the background at start. A retired id belongs to nobody, so nothing here can touch a
    /// workspace the user has.
    pub(crate) async fn sweep_retired(&self) {
        let retired: Vec<WorkspaceId> = self.state().retired.iter().cloned().collect();
        for id in retired {
            self.remove_left_sandbox(&id).await;
            self.forget_leftovers(&id, id.as_str()).await;
        }
    }

    /// Removes the sandbox a deleted workspace left, if there is one. A failure is a notice.
    async fn remove_left_sandbox(&self, id: &WorkspaceId) {
        let key = problem_key("left-sandbox", id);
        match self.inner.runtime.remove(&id.sandbox_name()).await {
            Ok(()) | Err(ComputeError::NotFound { .. }) => self.inner.problems.clear(&key),
            Err(e) => self.inner.problems.raise(Problem::new(
                key,
                format!("The sandbox of the deleted workspace {id} was not removed"),
                format!("{e}. Puddle tries again the next time it starts."),
            )),
        }
    }
}

/// Why a boot failed, for the user.
fn boot_message(e: &BootError) -> String {
    e.to_string()
}

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
                .map_err(TerminalError::Unavailable)?;
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
                move |stream| async move {
                    gated.serve_ssh(stream).await.map_err(|e| e.to_string())
                },
                move || host.terminal_gone(&named),
            )
            .await
        })
    }
}

impl<R: Runtime + Clone> WorkspaceService for HostWorkspaces<R> {
    fn local_checkout(&self, path: &str) -> Result<LocalSource, String> {
        let found = LocalCheckout::open(std::path::Path::new(path))?;
        // The runtime's own rule (owner, Puddle's folders) now, not only when the create runs.
        self.inner
            .runtime
            .check_checkout(&found.git_dir)
            .map_err(|e| match e {
                ComputeError::InvalidSpec { reason } => reason,
                other => other.to_string(),
            })?;
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
                    .map_err(|e| WorkspaceError::Unavailable(e.to_string()))?;
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
                    .map_err(|e| e.to_string())
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
                    Err(reason) => Attached::not_opened(reason),
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
                Err(reason) => Err(WorkspaceError::Unavailable(reason)),
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
mod room_for_a_grow;

#[cfg(test)]
mod kept_login_rows;

#[cfg(test)]
mod no_link_words;

#[cfg(test)]
mod download_wait_tests;

#[cfg(test)]
mod name_or_id_tests;
