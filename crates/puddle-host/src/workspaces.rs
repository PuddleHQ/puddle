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

use crate::agent_reports::ReportDesk;
use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::ops::{Deref, DerefMut};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use futures_util::future::BoxFuture;
use puddle_agent_proto::editor::{EditorServer, NEEDS_GLIBC};
use puddle_agent_proto::forward::OpenError;
use puddle_api::{
    DeleteCheck, DisplayName, EditorLogin, GitSetup, KeptLogin, LaunchError, Launcher, Listing,
    LocalCheckoutError, Operation, RepoFindings, Repository, StartFolder, WorkspaceError,
    WorkspaceRecord,
};
use puddle_boot::{Gate, GatedSandbox};
use puddle_ca::CaCertificate;
use puddle_compute::{ComputeError, ImageConfig, Runtime};
use puddle_lifecycle::Lifecycle;
use puddle_ports::{NoLink, Ports};
use puddle_proxy::Route;
use puddle_ssh::SshEndpoint;
use puddle_store::{Clock, Store};
use puddle_types::{
    CheckoutDir, DiskKind, EntryId, Event, EventSink, GuestEnv, GuestFile, GuestPath, Problem,
    Problems, SessionUser, WorkspaceId, WorkspaceStatus,
};

use crate::ssh_config::SshSetup;
use puddle_workspace::{Findings, Workspaces};
use tokio::sync::watch;
use tokio::task::JoinSet;

use crate::boot::BootKit;
use crate::files::{Stored, WorkspaceBook};
use crate::front::Flavours;
use crate::git_hosts::{Authors, SshListing};
use crate::injection::Injection;
use crate::op_error::{OpError, SAVE_HINT};

mod repositories;

pub(crate) mod orphans;

mod access;
mod boot;
mod create_and_grow;
mod plumbing;
mod service;
mod settle;
mod start_stop_delete;

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
    /// Whether the guest has its SSH agent socket: the switch as of this start.
    ssh_agent: bool,
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
    /// The user's variables in the sandbox's own spec: those it was made with, or the last set
    /// written to it. A sandbox reads them at its start, so a start with other variables writes
    /// them first (`Runtime::set_env`) and keeps the sandbox and its root disk.
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

/// What a failed create or start tells the user about what it could not clean up: the reason it
/// failed first, then what is still there and how it goes away.
fn with_leftovers(reason: OpError, leftovers: &[String]) -> OpError {
    if leftovers.is_empty() {
        return reason;
    }
    OpError::Cleanup {
        cause: Box::new(reason),
        leftovers: leftovers.join("; "),
    }
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
    restart_failure: Option<OpError>,
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

/// Why the runtime refuses a checkout, as the user is told: the reason of a refused spec as it is
/// (the runtime's own wording names the folder), any other failure by its message.
fn checkout_refusal(error: ComputeError) -> LocalCheckoutError {
    match error {
        ComputeError::InvalidSpec { reason } => LocalCheckoutError::Refused(reason),
        other => LocalCheckoutError::Refused(other.to_string()),
    }
}

#[cfg(test)]
mod download_wait_tests;

#[cfg(test)]
mod name_or_id_tests;

#[cfg(test)]
mod checkout_refusal_tests;
