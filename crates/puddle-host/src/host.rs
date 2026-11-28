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

use puddle_api::{EventHub, RunningApi, Services, SettingsRepo};
use puddle_certs::CorporateRoots;
use puddle_compute::Runtime;
use puddle_front::Front;
use puddle_fs::DataLock;
use puddle_lifecycle::{Lifecycle, ShutdownReport};
use puddle_netpolicy::{CompanySignals, LocalAccess, PuddleEndpoints};
use puddle_ports::Ports;
use puddle_proxy::{PortProbe, ProxyUrl, PullProxy, PullRoute, Resolver};
use puddle_secrets::{SecretStore, ToolPaths};
use puddle_services::{
    credentials::HostCredentials, network_health::HostNetworkHealth,
    network_health::PullProxyState, network_health::forward_network_changes,
    network_health::forward_proxy_sign_in_failures, proxy_sign_in::ProxySignInService,
    workspaces::Launcher,
};
use puddle_settings::resolve;
use puddle_store::{
    AllUsers, ConnectionRecorder, DEFAULT_SWEEP_PERIOD, Limits, Store, Sweeper, SystemClock,
};
use puddle_types::{Problems, WorkspaceId, WorkspaceStatus};
use puddle_upstream::{Chain, Discovery, Watching};
use puddle_workspace::Workspaces;
use tokio::sync::{Mutex as AsyncMutex, OnceCell};
use url::Url;

use crate::backup::DataFolderBackup;
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
use crate::restore::DataFolderRestore;
use crate::ssh_config::SshSetup;
use crate::workspaces::{Parts, RewriteGate};
use crate::{HostConfig, HostError, HostWorkspaces, ProxyPasswords, VsCodeLauncher};

mod background;
mod egress;
mod factory;
mod options;
mod phases;
mod reconcile;
mod serve;
mod steps;

use background::{Background, finish_retired_sweep};
use egress::{
    Egress, EgressParts, Injecting, company_signals, load_proxy_passwords, network_health_of,
    pem_of,
};
use phases::{
    Credentials, Opened, Outbound, Shared, State, build_outbound, build_services, open_credentials,
    open_runtime, open_state, reconcile_phase, serve_background, workspace_parts,
};
use reconcile::{adopt_holders, reconcile_problems, reconcile_with};
use serve::serve_api;

pub use factory::{MsbFactory, RuntimeFactory, RuntimeInputs};
pub use options::HostOptions;
pub use serve::RemoteListener;
pub use steps::{PREPARE_STEPS, Prepared, SHUTDOWN_STEPS, START_STEPS, Step, prepare};

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
    proxy: Arc<puddle_proxy::Proxy>,
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
    /// Told when a restore is confirmed.
    restore_requested: Arc<tokio::sync::Notify>,
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
        let problems = Arc::new(Problems::new(events.clone()));
        let restore_requested = Arc::new(tokio::sync::Notify::new());
        let clock = Arc::new(SystemClock);
        let State {
            store,
            all_users,
            settings,
            book,
            stored,
            retired,
            cleaned,
        } = open_state(&config.paths, &events, &problems, &clock, &mut steps)?;

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
            restore_requested: &restore_requested,
        };
        let listed: Vec<WorkspaceId> = stored.iter().map(|s| s.id.clone()).collect();
        let credentials = open_credentials(&shared, listed).await?;
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
        let serving = serve_background(&shared, &outbound, pull, &mut steps)?;

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
        let service = HostWorkspaces::new(parts, stored, retired, cleaned, &reconciled.status)?;
        let editor_windows = serving.front.windows().clone();
        let watchers = spawn_watchers(&shared, &service, &credentials.injecting, &editor_windows);
        steps.push(Step::WorkspacesReady);

        let services = build_services(&shared, &service, &credentials, &outbound, dev_cert);
        let served = serve_api(&config, services, &mut steps).await?;
        tracing::debug!(?steps, url = %served.url, "host start: every step ran");

        Ok(Self {
            info: served.info,
            url: served.url,
            remote_listener: served.remote_listener,
            events,
            problems,
            quit_trims: reconciled.quit_trims,
            carried: reconciled.carried,
            proxy_passwords: credentials.proxy_passwords,
            proxy: outbound.egress.proxy.clone(),
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
            restore_requested,
            _lock: lock,
        })
    }

    /// Resolves when the user has confirmed a restore. The restore applies at the next start, so a
    /// process that can start itself again (or tell its user to) should shut the host down now.
    /// A confirm that came before the call resolves it at once.
    pub async fn restore_requested(&self) {
        self.restore_requested.notified().await;
    }

    /// The signal [`Host::restore_requested`] waits on, for a caller that must not keep the host
    /// (and with it the data folder's lock) alive while it waits.
    #[must_use]
    pub fn restore_signal(&self) -> Arc<tokio::sync::Notify> {
        Arc::clone(&self.restore_requested)
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

    /// How many guest connections the proxy is serving right now, each one until its audit record
    /// is written (see [`puddle_proxy::Proxy::open_guest_streams`]). At zero the audit holds a
    /// record for every connection that has ended.
    #[must_use]
    pub fn open_guest_streams(&self) -> usize {
        self.proxy.open_guest_streams()
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
                let api = self.api.lock().await.take();
                if let Some(api) = api {
                    api.shutdown().await;
                }
                self.step(Step::ApiStopped);
                let background = self.background.lock().await.take();
                if let Some(mut background) = background {
                    background.sweeper.shutdown().await;
                    background.pull.shutdown().await;
                    background.front.close().await;
                    crate::vault::finish_sweep(
                        background.vault_sweep,
                        crate::vault::SWEEP_SHUTDOWN_WAIT,
                    )
                    .await;
                    finish_retired_sweep(
                        background.retired_sweep,
                        crate::vault::SWEEP_SHUTDOWN_WAIT,
                    )
                    .await;
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

#[cfg(test)]
mod tests;
