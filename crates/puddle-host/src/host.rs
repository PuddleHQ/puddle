// SPDX-License-Identifier: AGPL-3.0-or-later
//! The host: one start-up sequence and one shutdown sequence for the whole process.
//!
//! Starting has two phases because one step has to run before the process has any other thread:
//!
//! 1. [`prepare`] (synchronous): bind the image-pull proxy, pin the process environment, check
//!    the bundled runtime, read the corporate roots.
//! 2. [`Host::start`] (asynchronous): open the store, build the egress chain, open the runtime,
//!    reconcile with what an earlier run left, start the proxies and background tasks, build the
//!    workspace service and serve the API last, so nothing can call a half-built host.
//!
//! Every step is recorded as a [`Step`], in order, so tests (and a log reader) can see the
//! sequence that ran.

use crate::agent_reports::ReportDesk;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, PoisonError};

use puddle_api::{
    EventHub, HostCredentials, HostNetworkHealth, Launcher, ProxySignInService, RunningApi,
    Services, SettingsRepo, forward_network_changes, forward_proxy_sign_in_failures,
};
use puddle_certs::CorporateRoots;
use puddle_compute::Runtime;
use puddle_front::Front;
use puddle_fs::DataLock;
use puddle_lifecycle::{Lifecycle, ShutdownReport};
use puddle_netpolicy::{CompanySignals, LocalAccess, PuddleEndpoints};
use puddle_ports::Ports;
use puddle_proxy::{PortProbe, ProxyUrl, PullProxy, PullRoute, Resolver};
use puddle_secrets::{SecretStore, ToolPaths};
use puddle_settings::resolve;
use puddle_store::{
    AllUsers, ConnectionRecorder, DEFAULT_SWEEP_PERIOD, Limits, Store, Sweeper, SystemClock,
};
use puddle_types::{EventSink, Problems, WorkspaceId, WorkspaceStatus};
use puddle_upstream::{Chain, Discovery, Watching};
use puddle_workspace::Workspaces;
use tokio::sync::{Mutex as AsyncMutex, OnceCell};
use url::Url;

use crate::boot::BootKit;
use crate::dev_cert::{DevCertSource, host_dev_cert};
use crate::doctor::system_doctor;
use crate::files::{FileSettings, WorkspaceBook};
use crate::front::Flavours;
use crate::injection::InjectorFactory;
use crate::learned::HostLearned;
use crate::ports::{HostPorts, StorePins};
use crate::proxy_sign_ins::HostProxySignIns;
use crate::quit_trims::QuitTrims;
use crate::ssh_config::SshSetup;
use crate::workspaces::{Parts, RewriteGate};
use crate::{HostConfig, HostError, HostWorkspaces, ProxyPasswords, VsCodeLauncher};

mod egress;
mod factory;
mod reconcile;
mod serve;
mod steps;

use egress::{
    Egress, EgressParts, Injecting, company_signals, load_proxy_passwords, network_health_of,
    pem_of,
};
use reconcile::{adopt_holders, reconcile_problems, reconcile_with};
use serve::serve_api;

pub use factory::{MsbFactory, RuntimeFactory, RuntimeInputs};
pub use serve::RemoteListener;
pub use steps::{PREPARE_STEPS, Prepared, SHUTDOWN_STEPS, START_STEPS, Step, prepare};

/// What a host takes besides its configuration.
#[derive(Clone)]
#[non_exhaustive]
pub struct HostOptions {
    /// Opens the desktop editor for the workspaces: VS Code's `code` command by default.
    pub launcher: Arc<dyn Launcher>,
    /// The proxy discovery to use instead of the system's (tests).
    pub discovery: Option<Arc<Discovery>>,
    /// Builds, for each start of a workspace's sandbox, the injector that decides which credential
    /// is added to a request on a decrypted host. Without one a workspace gets Puddle's own Git
    /// injector (tests give another, for instance one that adds nothing).
    pub injector: Option<InjectorFactory>,
    /// Where the values of secrets are kept: the pasted tokens of identities and the secrets of
    /// workspace environments. Without one, the operating system's credential store (a value too
    /// long for one of its entries is spread over several). Tests give an in-memory store, so they
    /// never touch the real one.
    pub secret_store: Option<Arc<dyn SecretStore>>,
    /// Where the development certificate comes from: the first-run state and the certificate every
    /// workspace gets at boot. Without one, the host's own (its certificate stores and the data
    /// folder). Tests give a double.
    pub dev_cert: Option<Arc<dyn DevCertSource>>,
    /// Looks up the addresses of a name a rule allows, for the sandbox proxy's connects. Without
    /// one, the operating system's resolver. Tests give a double, so a name only a fake company
    /// proxy knows is never a question for the machine's real DNS.
    pub resolver: Option<Arc<dyn Resolver>>,
    /// What the host reads of the company network (proxy script, DNS suffixes, private-access
    /// clients, routes), shared by the sandbox proxy's guard and the API. Without one the host
    /// reads none, so no destination is ever in the company-network category. The shipped host
    /// uses [`HostOptions::system`], which reads the machine's own.
    pub company_signals: Option<Arc<dyn CompanySignals>>,
    /// Probes whether this machine can open `ssh.dev.azure.com:22`, for the refusal that says the
    /// company network blocks it. Without one, a real TCP probe; tests give a double so no test
    /// depends on the machine's network.
    pub port_probe: Option<Arc<dyn PortProbe>>,
    /// Holds an author rewrite back when it starts (tests, to make the rewrites of one workspace
    /// run out of order). Without one, no rewrite waits.
    pub rewrite_gate: Option<RewriteGate>,
    /// Serves Git over SSH for the Git hosts: SSH host keys for each start of a workspace's
    /// sandbox, listed in the guest's `ssh_known_hosts` under the Git hosts, and Puddle's own SSH
    /// server, which carries each command out over the host's HTTPS with the workspace's
    /// identities. On by default; off, SSH to a Git host is refused (`ssh_unsupported`).
    pub git_ssh: bool,
    /// Where new workspace ids come from. Without one, the operating system's random source
    /// ([`puddle_api::RandomIds`]); a test that writes ids into paths itself gives
    /// [`puddle_api::NameIds`], so a workspace called `acme` gets the id `acme`.
    pub workspace_ids: Option<Arc<dyn puddle_api::IdSource>>,
    /// Where a root disk grow reads the free space of the data folder's drive. Without one, the
    /// real drive; tests give a fixed figure so a grow never depends on the machine's disk.
    pub free_space: Option<Arc<dyn crate::Space>>,
    /// Looks up a name for the host's own connections (the lists of repositories, the sign-in
    /// checks) before the company proxy is asked. Without one, the operating system's resolver;
    /// tests give a double, so a name is never a question for the machine's real DNS.
    pub host_lookup: Option<puddle_upstream::HostLookup>,
    /// Resolves the names of the front in front of Puddle for the guard. Without one, the
    /// operating system's resolver; tests give a double.
    pub front_resolver: Option<Arc<dyn puddle_api::NameResolver>>,
    /// The folders searched for the Azure CLI by the system check (a `PATH` value). Without one,
    /// the process's `PATH`; tests give an empty one, so the check never runs the machine's real
    /// `az`, which waits on the network.
    pub az_search_path: Option<std::ffi::OsString>,
}

impl Default for HostOptions {
    fn default() -> Self {
        Self {
            launcher: Arc::new(VsCodeLauncher::new()),
            discovery: None,
            injector: None,
            secret_store: None,
            dev_cert: None,
            resolver: None,
            company_signals: None,
            port_probe: None,
            rewrite_gate: None,
            git_ssh: true,
            workspace_ids: None,
            free_space: None,
            host_lookup: None,
            front_resolver: None,
            az_search_path: None,
        }
    }
}

impl HostOptions {
    /// The options of the shipped host: the defaults, plus the company-network signals the
    /// machine itself gives ([`crate::HostSignals::system`]; none on a platform without readers).
    /// Tests start from [`HostOptions::default`], so a test never depends on the machine's network.
    #[must_use]
    pub fn system() -> Self {
        Self {
            company_signals: Some(crate::HostSignals::system()),
            ..Self::default()
        }
    }
}

impl std::fmt::Debug for HostOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostOptions").finish_non_exhaustive()
    }
}

/// What a shutdown did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostShutdown {
    /// The trim and stop of every sandbox.
    pub sandboxes: ShutdownReport,
    /// How many workspace operations were still running after the grace period and were ended.
    pub operations_ended: usize,
    /// The steps that ran.
    pub steps: Vec<Step>,
}

struct Background {
    sweeper: Sweeper,
    // Holds the store; awaited at shutdown so the database is closed when the host is.
    vault_sweep: tokio::task::JoinHandle<()>,
    // Also holds the store (the injection removes rows); awaited at shutdown for the same reason.
    retired_sweep: tokio::task::JoinHandle<()>,
    pull: PullRoute,
    front: Front,
    // Dropped (stopped) with the rest; `None` where the OS cannot watch.
    _watching: Option<Watching>,
    // Tells the API's event stream about each new network epoch; ended with the rest.
    network_events: AbortOnDrop,
    sign_in_failures: AbortOnDrop,
    // Applies a change of a running workspace's identities to what it decrypts and its authors.
    git_changes: AbortOnDrop,
    // Turns a read that needs a sign-in into the user's notice.
    sign_in_notices: AbortOnDrop,
    // Asks Microsoft's editor server to switch to a staged build when no window is on it.
    editor_updates: AbortOnDrop,
    // Says when the free space on the drive that holds the data folder crosses a line.
    space_watch: AbortOnDrop,
    // Reads the disks' use now and then and warns when one reaches the threshold.
    disk_watch: AbortOnDrop,
}

impl Background {
    fn new(serving: Serving, watchers: Watchers, vault_sweep: tokio::task::JoinHandle<()>) -> Self {
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

/// Aborts its task when dropped, so a forwarder never outlives the host.
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl AbortOnDrop {
    /// Announces each new network epoch of `discovery` on `events`.
    fn forwarding(discovery: &Discovery, events: &Arc<EventHub>) -> Self {
        Self(forward_network_changes(
            discovery,
            events.clone() as Arc<dyn EventSink>,
        ))
    }
}

impl AbortOnDrop {
    /// Announces on `events` each time signing in to a company proxy begins to fail.
    fn forwarding_sign_in_failures(chain: &Chain, events: &Arc<EventHub>) -> Self {
        Self(forward_proxy_sign_in_failures(
            chain,
            events.clone() as Arc<dyn EventSink>,
        ))
    }
}

impl AbortOnDrop {
    /// Aborts the task and waits until it is gone: an abort only takes effect when the runtime
    /// next polls the task, and what the task holds (a store handle) is released then.
    async fn stop(&mut self) {
        self.0.abort();
        let _ = (&mut self.0).await;
    }
}

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// The running host. Its API URL and token are what a window or the CLI connects with.
pub struct Host<R: Runtime + Clone> {
    info: puddle_api::ConnectionInfo,
    url: Url,
    remote_listener: Option<RemoteListener>,
    events: Arc<EventHub>,
    problems: Arc<Problems>,
    quit_trims: QuitTrims,
    /// The untrimmed disks the last quit left, with why; the ones still untrimmed at this quit
    /// stay in the file.
    carried: BTreeMap<WorkspaceId, String>,
    proxy_passwords: Arc<ProxyPasswords>,
    store: Arc<Store>,
    settings: Arc<dyn SettingsRepo>,
    endpoints: PuddleEndpoints,
    workspaces: HostWorkspaces<R>,
    editor_windows: puddle_front::Windows,
    lifecycle: Arc<Lifecycle<R>>,
    grace: std::time::Duration,
    api: AsyncMutex<Option<RunningApi>>,
    background: AsyncMutex<Option<Background>>,
    steps: Mutex<Vec<Step>>,
    reconcile: puddle_lifecycle::ReconcileReport,
    stopped: OnceCell<HostShutdown>,
    // Held until the host is dropped, after shutdown has stopped the sandboxes.
    _lock: DataLock,
}

impl<R: Runtime + Clone> std::fmt::Debug for Host<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Host")
            .field("url", &self.url)
            .finish_non_exhaustive()
    }
}

/// The local-destination settings of `workspace`, read from the settings at each connection so a
/// change applies to the next one. Settings that cannot be read leave every local destination
/// blocked (the guard fails closed) and are logged with the reason; the API shows the same damage
/// on the workspace.
fn local_access(settings: &dyn SettingsRepo, workspace: &WorkspaceId) -> LocalAccess {
    match local_access_checked(settings, workspace) {
        Ok(access) => access,
        Err(reason) => {
            tracing::warn!(%workspace, %reason, "local destinations stay blocked: the settings cannot be read");
            LocalAccess::UNREADABLE
        }
    }
}

fn local_access_checked(
    settings: &dyn SettingsRepo,
    workspace: &WorkspaceId,
) -> Result<LocalAccess, crate::settings_read::SettingsReadError> {
    let global = crate::settings_read::global(settings)?;
    let own = crate::settings_read::workspace(settings, workspace)?;
    Ok(LocalAccess::from_effective(&resolve(&global, Some(&own))))
}

impl<R: Runtime + Clone> Host<R> {
    /// Starts the host: the asynchronous phase, after [`prepare`].
    ///
    /// # Errors
    ///
    /// [`HostError`] from the step that failed. Whatever started is stopped again by drop, and
    /// no sandbox is left running: reconcile only runs after everything that can refuse to start
    /// has been built.
    pub async fn start<F>(
        prepared: Prepared,
        factory: &F,
        options: HostOptions,
    ) -> Result<Self, HostError>
    where
        F: RuntimeFactory<Runtime = R>,
    {
        let Prepared {
            lock,
            config,
            pull,
            endpoints,
            roots,
            mut steps,
        } = prepared;

        // Store, settings and the event hub.
        let events = Arc::new(EventHub::default());
        let problems = Arc::new(Problems::new(events.clone() as Arc<dyn EventSink>));
        let clock = Arc::new(SystemClock);
        let State {
            store,
            all_users,
            settings,
            book,
            stored,
            retired,
        } = open_state(&config.paths, &events, &clock, &mut steps)?;

        let shared = Shared {
            config: &config,
            options: &options,
            events: &events,
            problems: &problems,
            clock: &clock,
            store: &store,
            settings: &settings,
            endpoints: &endpoints,
            roots: &roots,
        };
        let credentials = open_credentials(&shared)?;
        let (outbound, pull) = build_outbound(
            &shared,
            &credentials.injecting,
            &credentials.proxy_passwords,
            pull,
        );
        steps.push(Step::UpstreamBuilt);

        let opened = open_runtime(&shared, &outbound, factory, &mut steps).await?;

        // Clean up after an earlier run, then rebuild what only lives in memory.
        let reconciled = reconcile_phase(&opened.runtime, &config, &stored, &problems).await?;
        steps.push(Step::Reconciled);
        let workspaces = adopt_holders(&opened.runtime, &config, &stored).await?;
        steps.push(Step::WorkspacesAdopted);

        // From here the host serves.
        let serving = serve_background(&shared, &outbound, pull, &mut steps).await?;

        let lifecycle = Arc::new(Lifecycle::new(
            opened.runtime.clone(),
            config.shutdown.clone(),
        ));
        let dev_cert = opened.dev_cert.clone();
        let parts = workspace_parts(
            &shared,
            &outbound,
            &credentials.injecting,
            &serving,
            opened,
            (workspaces, book, all_users),
            lifecycle.clone(),
        );
        let service = HostWorkspaces::new(parts, stored, retired, &reconciled.status)?;
        let editor_windows = serving.front.windows().clone();
        let watchers = spawn_watchers(&shared, &service, &credentials.injecting, &editor_windows);
        steps.push(Step::WorkspacesReady);

        let services = build_services(
            &shared,
            &service,
            &credentials.injecting,
            &outbound,
            credentials.vault,
            dev_cert,
        );
        let served = serve_api(&config, services, &mut steps).await?;

        Ok(Self {
            info: served.info,
            url: served.url,
            remote_listener: served.remote_listener,
            events,
            problems,
            quit_trims: reconciled.quit_trims,
            carried: reconciled.carried,
            proxy_passwords: credentials.proxy_passwords,
            store,
            settings,
            endpoints,
            workspaces: service,
            editor_windows,
            lifecycle,
            grace: config.operations_grace,
            api: AsyncMutex::new(Some(served.api)),
            background: AsyncMutex::new(Some(Background::new(
                serving,
                watchers,
                credentials.vault_sweep,
            ))),
            steps: Mutex::new(steps),
            reconcile: reconciled.report,
            stopped: OnceCell::new(),
            _lock: lock,
        })
    }

    /// The API's origin, `http://127.0.0.1:<port>/`.
    #[must_use]
    pub fn url(&self) -> &Url {
        &self.url
    }

    /// The network listener for a reverse proxy on another machine, when remote serving has one.
    #[must_use]
    pub fn remote_listener(&self) -> Option<&RemoteListener> {
        self.remote_listener.as_ref()
    }

    /// The API token. Never log it.
    #[must_use]
    pub fn token(&self) -> &str {
        self.info.token.expose()
    }

    /// The connection (URL and token), for writing a connection file.
    #[must_use]
    pub fn connection(&self) -> &puddle_api::ConnectionInfo {
        &self.info
    }

    /// The event hub the API streams from; the tray and notifications subscribe through it.
    #[must_use]
    pub fn events(&self) -> &Arc<EventHub> {
        &self.events
    }

    /// The background problems the API lists at `GET /api/problems`.
    #[must_use]
    pub fn problems(&self) -> &Arc<Problems> {
        &self.problems
    }

    /// The sign-ins for company proxies that ask for a user name and password, kept in the
    /// operating system's credential store. A change is used by the next connection.
    #[must_use]
    pub fn proxy_passwords(&self) -> &Arc<ProxyPasswords> {
        &self.proxy_passwords
    }

    /// The database.
    #[must_use]
    pub fn store(&self) -> &Arc<Store> {
        &self.store
    }

    /// The settings documents the API serves, for the desktop shell's own reads (what its tray and
    /// notifications do follow them).
    #[must_use]
    pub fn settings(&self) -> &Arc<dyn SettingsRepo> {
        &self.settings
    }

    /// Every address Puddle itself listens on, which the destination guard keeps guests away
    /// from. Components that open their own listener (the forwarder) register it here.
    #[must_use]
    pub fn endpoints(&self) -> &PuddleEndpoints {
        &self.endpoints
    }

    /// The workspace service the API serves.
    #[must_use]
    pub fn workspaces(&self) -> &HostWorkspaces<R> {
        &self.workspaces
    }

    /// The browser editor windows the front counts per workspace: a window keeps a connection to
    /// its editor while it is open. A server update waits until a workspace has none.
    #[must_use]
    pub fn editor_windows(&self) -> &puddle_front::Windows {
        &self.editor_windows
    }

    /// What reconcile found and did at start.
    #[must_use]
    pub fn reconcile_report(&self) -> &puddle_lifecycle::ReconcileReport {
        &self.reconcile
    }

    /// The steps run so far, in order.
    #[must_use]
    pub fn steps(&self) -> Vec<Step> {
        self.steps
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn step(&self, step: Step) {
        self.steps
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(step);
    }

    /// Writes the workspaces whose disk is still untrimmed: this quit's failed trims, and the
    /// ones from the last quit that no later trim has fixed (their problem still stands).
    fn save_quit_trims(&self, report: &ShutdownReport) {
        let standing: BTreeSet<String> = self.problems.list().into_iter().map(|p| p.key).collect();
        let mut untrimmed: BTreeMap<WorkspaceId, String> = self
            .carried
            .iter()
            .filter(|(id, _)| standing.contains(&crate::quit_trims::key(id)))
            .map(|(id, why)| (id.clone(), why.clone()))
            .collect();
        for (sandbox, why) in report.untrimmed() {
            if let Ok(id) = WorkspaceId::new(sandbox.as_str()) {
                untrimmed.insert(id, why);
            }
        }
        // A workspace whose trim worked now is clean, whatever the file said.
        for outcome in &report.sandboxes {
            if outcome.trim == puddle_lifecycle::TrimOutcome::Trimmed
                && let Ok(id) = WorkspaceId::new(outcome.sandbox.as_str())
            {
                untrimmed.remove(&id);
            }
        }
        self.quit_trims.save(&untrimmed);
    }

    /// Stops the host: refuse new operations, let running ones finish (up to the grace period),
    /// trim and stop every sandbox, close the routes, stop the API, stop the background tasks.
    /// Calling it again returns the first result.
    pub async fn shutdown(&self) -> HostShutdown {
        self.stopped
            .get_or_init(|| async {
                self.workspaces.close();
                self.step(Step::WorkspacesClosed);
                let operations_ended = self.workspaces.finish_operations(self.grace).await;
                self.step(Step::OperationsFinished);
                let sandboxes = self.lifecycle.shutdown().await;
                self.step(Step::WorkspacesStopped);
                self.save_quit_trims(&sandboxes);
                self.workspaces.release_all().await;
                self.step(Step::RoutesClosed);
                if let Some(api) = self.api.lock().await.take() {
                    api.shutdown().await;
                }
                self.step(Step::ApiStopped);
                if let Some(mut background) = self.background.lock().await.take() {
                    background.sweeper.shutdown().await;
                    background.pull.shutdown().await;
                    background.front.close().await;
                    crate::vault::finish_sweep(
                        background.vault_sweep,
                        crate::vault::SWEEP_SHUTDOWN_WAIT,
                    )
                    .await;
                    let _ = background.retired_sweep.await;
                    for task in [
                        &mut background.network_events,
                        &mut background.sign_in_failures,
                        &mut background.git_changes,
                        &mut background.sign_in_notices,
                        &mut background.editor_updates,
                        &mut background.space_watch,
                        &mut background.disk_watch,
                    ] {
                        task.stop().await;
                    }
                }
                self.step(Step::BackgroundStopped);
                let steps = self
                    .steps()
                    .into_iter()
                    .skip_while(|s| !matches!(s, Step::WorkspacesClosed))
                    .collect();
                HostShutdown {
                    sandboxes,
                    operations_ended,
                    steps,
                }
            })
            .await
            .clone()
    }
}

/// What `open_state` opened.
struct State {
    store: Arc<Store>,
    /// The reach across every user's data, taken before anything serves.
    all_users: AllUsers,
    settings: Arc<dyn SettingsRepo>,
    book: WorkspaceBook,
    stored: Vec<crate::files::Stored>,
    retired: BTreeSet<WorkspaceId>,
}

/// Opens the database, the settings files and the workspace list.
fn open_state(
    paths: &crate::HostPaths,
    events: &Arc<EventHub>,
    clock: &Arc<SystemClock>,
    steps: &mut Vec<Step>,
) -> Result<State, HostError> {
    crate::data_folder::make_owner_only(paths)?;
    let store = Arc::new(
        Store::open(&paths.store(), clock.clone(), Limits::default())?
            .with_events(events.clone() as Arc<dyn EventSink>),
    );
    // Taken here, before the store reaches anything that serves requests, so no request handler
    // can take it after.
    let all_users = store.take_all_users().ok_or(HostError::State {
        what: "the database",
        reason: "the reach across every user was taken before the host started".to_owned(),
    })?;
    let settings: Arc<dyn SettingsRepo> = Arc::new(FileSettings::new(paths.settings()));
    let book = WorkspaceBook::new(paths.workspace_book());
    let crate::files::Book {
        workspaces: stored,
        retired,
    } = book.load()?;
    steps.push(Step::StoreOpened);
    Ok(State {
        store,
        all_users,
        settings,
        book,
        stored,
        retired,
    })
}

/// The handles every start-up phase reads: the settings it has opened and the options it was given.
struct Shared<'a> {
    config: &'a HostConfig,
    options: &'a HostOptions,
    events: &'a Arc<EventHub>,
    problems: &'a Arc<Problems>,
    clock: &'a Arc<SystemClock>,
    store: &'a Arc<Store>,
    settings: &'a Arc<dyn SettingsRepo>,
    endpoints: &'a PuddleEndpoints,
    roots: &'a CorporateRoots,
}

/// The way out through the company network, with what the host's screens read about it.
struct Outbound {
    connections: Arc<ConnectionRecorder>,
    reports: Arc<ReportDesk>,
    ports: Ports,
    ssh_accounts: Arc<crate::ssh_accounts::SshAccounts>,
    egress: Egress,
    sign_ins: Arc<dyn ProxySignInService>,
    network_health: Arc<HostNetworkHealth>,
    pull_url: ProxyUrl,
}

/// The way out: the company network (discovery, sign-in as the user, then Basic) for the sandbox
/// proxy and the pull proxy alike. Returns the pull proxy, now going out through it.
fn build_outbound(
    shared: &Shared<'_>,
    injecting: &Injecting,
    proxy_passwords: &Arc<ProxyPasswords>,
    pull: PullProxy,
) -> (Outbound, PullProxy) {
    let Shared {
        config,
        options,
        events,
        problems,
        clock,
        store,
        settings,
        endpoints,
        roots,
    } = *shared;
    let connections = Arc::new(ConnectionRecorder::new(store.clone(), problems.clone()));
    let reports = Arc::new(ReportDesk::new(problems.clone()));
    let ports = Ports::new(
        endpoints.clone(),
        Arc::new(StorePins::new(store.clone())),
        events.clone() as Arc<dyn EventSink>,
    );
    // Who `ssh -T` greets: read from the store, named once the repository reader exists.
    let ssh_accounts = Arc::new(crate::ssh_accounts::SshAccounts::new(store.clone()));
    let mut egress_parts = EgressParts::new(
        injecting.termination(),
        proxy_passwords,
        &reports,
        &ports,
        roots,
    );
    if options.git_ssh {
        egress_parts.git_ssh = Some(ssh_accounts.clone());
    }
    let egress = Egress::build(
        config,
        options,
        settings,
        endpoints,
        &connections,
        events,
        egress_parts,
    );
    let sign_ins: Arc<dyn ProxySignInService> = Arc::new(HostProxySignIns::new(
        proxy_passwords.clone(),
        egress.chain.clone(),
    ));
    let network_health = network_health_of(
        &egress.chain,
        &egress.discovery,
        clock,
        roots,
        sign_ins.clone(),
        company_signals(options),
    );
    network_health.set_tls_left_out(injecting.tls.rejected_roots(), roots);
    let pull_url = pull.proxy_url();
    // Image pulls are Puddle's own traffic: audited with origin `puddle`, like sandbox traffic.
    let pull = pull
        .with_company_signals(company_signals(options))
        .with_upstream(egress.upstream.clone())
        .with_connection_log(connections.clone());
    let outbound = Outbound {
        connections,
        reports,
        ports,
        ssh_accounts,
        egress,
        sign_ins,
        network_health,
        pull_url,
    };
    (outbound, pull)
}

/// What the credential injection and the proxy sign-ins stand on.
struct Credentials {
    vault: Arc<dyn SecretStore>,
    vault_sweep: tokio::task::JoinHandle<()>,
    injecting: Injecting,
    proxy_passwords: Arc<ProxyPasswords>,
}

/// Credential injection: what each workspace decrypts, the injector and the secrets behind it,
/// and the client that verifies the real servers of decrypted hosts.
fn open_credentials(shared: &Shared<'_>) -> Result<Credentials, HostError> {
    let Shared {
        config,
        options,
        events,
        problems,
        store,
        settings,
        roots,
        ..
    } = *shared;
    let vault: Arc<dyn SecretStore> = options
        .secret_store
        .clone()
        .unwrap_or_else(crate::vault::os_vault);
    crate::vault::check_in_background(options.secret_store.clone(), problems.clone());
    let vault_sweep =
        crate::vault::sweep_in_background(vault.clone(), store.clone(), problems.clone());
    let marks = config.paths.login_markers();
    let injecting = Injecting::build(options, &vault, store, events, roots, marks, settings)?;
    let proxy_passwords = ProxyPasswords::new(vault.clone());
    load_proxy_passwords(proxy_passwords.clone(), problems.clone());
    Ok(Credentials {
        vault,
        vault_sweep,
        injecting,
        proxy_passwords,
    })
}

/// The guests' boot files and the runtime that boots them.
struct Opened<R> {
    dev_cert: Arc<dyn DevCertSource>,
    kit: BootKit,
    ssh: Arc<SshSetup>,
    runtime: R,
}

async fn open_runtime<R, F>(
    shared: &Shared<'_>,
    outbound: &Outbound,
    factory: &F,
    steps: &mut Vec<Step>,
) -> Result<Opened<R>, HostError>
where
    R: Runtime,
    F: RuntimeFactory<Runtime = R>,
{
    let Shared {
        config,
        options,
        clock,
        store,
        roots,
        ..
    } = *shared;
    // One development certificate service: the guests' boot files and the first-run line.
    let dev_cert: Arc<dyn DevCertSource> = options
        .dev_cert
        .clone()
        .unwrap_or_else(|| host_dev_cert(config.paths.data(), clock.clone()));
    let kit = BootKit::new(
        &config.paths.guest_share(),
        &config.guest,
        outbound.egress.proxy.clone(),
        roots,
        store.clone(),
        config.paths.data(),
    )?
    .with_dev_cert(dev_cert.clone());
    steps.push(Step::GuestFilesReady);

    // The login key of direct SSH: made before the runtime opens, which takes its public half.
    let ssh = Arc::new(SshSetup::open(config.paths.data(), &config.ssh));

    // The runtime, with image pulls through the pull proxy and the corporate roots trusted.
    let runtime = factory
        .open(RuntimeInputs {
            layout: &config.layout,
            guest_share: config.paths.guest_share(),
            data: config.paths.data().to_path_buf(),
            pull_proxy: outbound.pull_url.clone(),
            registry_roots: pem_of(roots),
            log_level: config.runtime_log_level.clone(),
            ssh_login_key: ssh.authorized_key().map(str::to_owned),
        })
        .await?;
    steps.push(Step::RuntimeOpened);
    Ok(Opened {
        dev_cert,
        kit,
        ssh,
        runtime,
    })
}

/// The parts of the host that serve beside the API.
struct Serving {
    pull: PullRoute,
    sweeper: Sweeper,
    watching: Option<Watching>,
    network_events: AbortOnDrop,
    sign_in_failures: AbortOnDrop,
    flavours: Arc<Flavours>,
    front: Front,
    front_guests: Arc<dyn puddle_front::Guests>,
    front_halves: Arc<dyn puddle_front::ServerHalves>,
}

/// The pull proxy first: the first create pulls an image. Then the sweeper, the network watch and
/// the browser editor's front.
async fn serve_background(
    shared: &Shared<'_>,
    outbound: &Outbound,
    pull: PullProxy,
    steps: &mut Vec<Step>,
) -> Result<Serving, HostError> {
    let pull = pull.serve().map_err(HostError::PullProxy)?;
    // Image pulls leave through the company proxy chain, like the sandboxes' traffic.
    outbound.network_health.set_pull_proxy(true, true);
    steps.push(Step::PullProxyServing);
    let sweeper = Sweeper::spawn_reporting(
        shared.store.clone(),
        DEFAULT_SWEEP_PERIOD,
        shared.problems.clone(),
    );
    let watching = outbound.egress.discovery.watch();
    let network_events = AbortOnDrop::forwarding(&outbound.egress.discovery, shared.events);
    let sign_in_failures =
        AbortOnDrop::forwarding_sign_in_failures(&outbound.egress.chain, shared.events);
    // The browser editor's front: a listener on this computer's loopback that reaches each
    // running workspace's editor through its agent link, so a window can open it by origin.
    let flavours = Arc::new(Flavours::default());
    let (front, front_guests, front_halves) = crate::front::serve(
        &shared.config.paths,
        shared.endpoints,
        outbound.ports.clone(),
        flavours.clone(),
        shared.problems,
    )
    .await?;
    steps.push(Step::BackgroundStarted);
    Ok(Serving {
        pull,
        sweeper,
        watching,
        network_events,
        sign_in_failures,
        flavours,
        front,
        front_guests,
        front_halves,
    })
}

/// What a run of the host left unfinished, and what it learned about it: the reconcile report, the
/// state of each known sandbox and the trims that failed when the last run quit.
struct Reconciled {
    report: puddle_lifecycle::ReconcileReport,
    status: BTreeMap<WorkspaceId, WorkspaceStatus>,
    quit_trims: QuitTrims,
    carried: BTreeMap<WorkspaceId, String>,
}

/// Cleans up after an earlier run, then reads what only lives in memory back in.
async fn reconcile_phase<R: Runtime>(
    runtime: &R,
    config: &HostConfig,
    stored: &[crate::files::Stored],
    problems: &Problems,
) -> Result<Reconciled, HostError> {
    let (report, status) = reconcile_with(runtime, config, stored).await?;
    for problem in reconcile_problems(&report) {
        problems.raise(problem);
    }
    // A trim that failed when the last run quit is told now, to a user who can read it.
    let quit_trims = QuitTrims::new(config.paths.quit_trims());
    let carried: BTreeMap<WorkspaceId, String> = quit_trims
        .load()
        .into_iter()
        .filter(|(id, _)| stored.iter().any(|s| &s.id == id))
        .collect();
    for (id, why) in &carried {
        if let Some(stored) = stored.iter().find(|s| &s.id == id) {
            problems.raise(crate::quit_trims::problem(id, stored.name.as_str(), why));
        }
    }
    Ok(Reconciled {
        report,
        status,
        quit_trims,
        carried,
    })
}

/// What the workspace service is made of, from the phases before it.
fn workspace_parts<R: Runtime + Clone>(
    shared: &Shared<'_>,
    outbound: &Outbound,
    injecting: &Injecting,
    serving: &Serving,
    opened: Opened<R>,
    (workspaces, book, all_users): (Workspaces, WorkspaceBook, AllUsers),
    lifecycle: Arc<Lifecycle<R>>,
) -> Parts<R> {
    let Shared {
        config,
        options,
        events,
        problems,
        clock,
        store,
        settings,
        ..
    } = *shared;
    Parts {
        runtime: opened.runtime,
        workspaces,
        lifecycle,
        kit: opened.kit,
        events: events.clone() as Arc<dyn EventSink>,
        clock: clock.clone(),
        settings: settings.clone(),
        store: store.clone(),
        all_users,
        launcher: options.launcher.clone(),
        ssh: opened.ssh,
        book,
        injection: injecting.injection.clone(),
        problems: problems.clone(),
        reports: outbound.reports.clone(),
        ports: outbound.ports.clone(),
        rewrite_gate: options.rewrite_gate.clone(),
        flavours: serving.flavours.clone(),
        front_addr: serving.front.local_addr(),
        front_guests: serving.front_guests.clone(),
        front_halves: serving.front_halves.clone(),
        ids: options
            .workspace_ids
            .clone()
            .unwrap_or_else(|| Arc::new(puddle_api::RandomIds)),
        login_markers: config.paths.login_markers(),
        space: crate::space_watch::space_or_drive(options.free_space.clone(), config.paths.data()),
    }
}

/// The tasks that keep running beside the workspaces, each stopped when it drops.
struct Watchers {
    retired_sweep: tokio::task::JoinHandle<()>,
    git_changes: AbortOnDrop,
    sign_in_notices: AbortOnDrop,
    editor_updates: AbortOnDrop,
    space_watch: AbortOnDrop,
    disk_watch: AbortOnDrop,
}

fn spawn_watchers<R: Runtime + Clone>(
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
        crate::space_watch::DriveOf(shared.config.paths.data().to_path_buf()),
        shared.config.free_space,
        shared.settings.clone(),
        shared.events.clone() as Arc<dyn EventSink>,
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

/// The services the API serves, wired to the host's stores and workspaces.
fn build_services<R: Runtime + Clone>(
    shared: &Shared<'_>,
    service: &HostWorkspaces<R>,
    injecting: &Injecting,
    outbound: &Outbound,
    vault: Arc<dyn SecretStore>,
    dev_cert: Arc<dyn DevCertSource>,
) -> Services {
    let Shared {
        config,
        options,
        events,
        problems,
        clock,
        store,
        settings,
        endpoints,
        ..
    } = *shared;
    let services = Services::new(
        store.clone(),
        settings.clone(),
        events.clone(),
        clock.clone(),
    )
    .with_problems(problems.clone())
    .with_workspaces(Arc::new(service.clone()))
    .with_network_health(outbound.network_health.clone())
    .with_secret_store(vault.clone())
    .with_credentials(Arc::new(
        HostCredentials::new(ToolPaths::resolve(), vault)
            .with_read_log(Arc::clone(&injecting.reads)),
    ))
    .with_repos({
        let repos = injecting.repos(
            &outbound.egress.chain,
            clock,
            outbound.connections.clone(),
            events,
        );
        outbound.ssh_accounts.read_with(repos.clone());
        repos
    })
    .with_doctor(system_doctor(
        &config.layout,
        &config.paths,
        &config.expected_runtime,
        settings.clone(),
        options.az_search_path.clone(),
    ))
    .with_dev_cert(dev_cert)
    .with_ports(Arc::new(HostPorts::new(outbound.ports.clone())))
    .with_terminals(Arc::new(service.clone()))
    .with_learned(Arc::new(HostLearned::new(
        injecting.terminations.clone(),
        Arc::clone(store),
    )))
    .with_proxy_sign_ins(outbound.sign_ins.clone())
    .with_company_signals(company_signals(options))
    .with_endpoints(endpoints.clone());
    let services = match options.front_resolver.clone() {
        Some(resolver) => services.with_front_resolver(resolver),
        None => services,
    };
    // Browsers sign in against the database's secrets, which only the API ever holds.
    match store.take_account_secrets() {
        Some(secrets) => services.with_account_secrets(secrets),
        None => services,
    }
}

#[cfg(test)]
mod tests;
