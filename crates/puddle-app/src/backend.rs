// SPDX-License-Identifier: GPL-3.0-or-later
//! The backend the window talks to, in this process: the Puddle host, the same one `puddle serve`
//! runs (store, proxies, sandbox runtime, workspaces, API), or the in-memory fixture for developing
//! the window.
//!
//! [`Source::prepare`] runs while the process still has one thread (the host pins the process
//! environment, which is only sound then); [`Source::start`] starts the backend inside the app's
//! async runtime. [`Backend`] is what the shell keeps: the origin, the token, the event hub, the
//! problem list and the shutdown.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use futures_util::future::BoxFuture;
use puddle_api::{
    ConnectionFileError, ConnectionInfo, EventHub, ServeError, SettingsRepo, WorkspaceService,
};
use puddle_host::{
    Host, HostConfig, HostError, HostOptions, HostShutdown, MsbFactory, Platform, Prepared,
    RuntimeFactory, prepare,
};
use puddle_store::Store;
use puddle_types::{Problems, WorkspaceId};
use tokio::sync::Mutex;
use url::Url;

use crate::shell::Options;

/// Why the backend didn't start.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BackendError {
    /// The fixture was asked for, but this build doesn't have it.
    #[error("this build has no fixture backend: build with the `fixture` feature")]
    NoFixture,
    /// The host didn't start.
    #[error(transparent)]
    Host(#[from] HostError),
    /// The fixture's API couldn't listen.
    #[error(transparent)]
    Serve(#[from] ServeError),
    /// The fixture's store couldn't be opened or seeded.
    #[error("cannot set up the fixture: {0}")]
    Fixture(String),
    /// The token couldn't be made, or the connection file couldn't be written.
    #[error(transparent)]
    Connection(#[from] ConnectionFileError),
    /// The API reported an address that isn't a URL.
    #[error("the API's address is not a URL: {0}")]
    Url(#[from] url::ParseError),
}

/// What the shell runs behind its window.
#[derive(Debug)]
pub enum Source {
    /// The Puddle host, after its synchronous phase ([`prepare`]), or why that failed. The error
    /// is reported when the window would open: by then the single-instance check has handed a
    /// second launch to the running app, which holds the data folder this one could not lock.
    Host(Box<Result<Prepared, HostError>>),
    /// The in-memory fixture: the real API on in-memory services with one seeded pending request.
    Fixture,
}

impl Source {
    /// The backend `options` ask for, with the host's synchronous phase run on `platform`. Call
    /// it before the process has a second thread.
    #[must_use]
    pub fn prepare(options: &Options, platform: &dyn Platform) -> Self {
        if options.fixture {
            return Self::Fixture;
        }
        Self::Host(Box::new(
            host_config(options).and_then(|config| prepare(config, platform)),
        ))
    }

    /// Starts the backend: the host on the bundled runtime, or the fixture.
    ///
    /// # Errors
    ///
    /// [`BackendError`] when it can't start.
    pub async fn start(self, options: &Options) -> Result<Backend, BackendError> {
        self.start_with(options, &MsbFactory, HostOptions::default())
            .await
    }

    /// [`Source::start`] with the host's runtime opened by `factory` and given `host` (tests open
    /// a fake runtime).
    ///
    /// # Errors
    ///
    /// As [`Source::start`].
    pub async fn start_with<F: RuntimeFactory>(
        self,
        options: &Options,
        factory: &F,
        host: HostOptions,
    ) -> Result<Backend, BackendError>
    where
        F::Runtime: 'static,
    {
        match self {
            Self::Host(prepared) => {
                let host = Host::start((*prepared)?, factory, host).await?;
                Ok(Backend::of_host(host)?)
            }
            #[cfg(feature = "fixture")]
            Self::Fixture => {
                let backend = crate::fixture::start(options.port).await?;
                if let Some(file) = &options.connection_file {
                    backend.info.write(file)?;
                }
                Ok(backend)
            }
            #[cfg(not(feature = "fixture"))]
            Self::Fixture => {
                let _ = options;
                Err(BackendError::NoFixture)
            }
        }
    }
}

/// The host's configuration: installed beside this program, plus what `options` change.
fn host_config(options: &Options) -> Result<HostConfig, HostError> {
    let exe = std::env::current_exe().map_err(HostError::ProgramFolder)?;
    let mut config = match &options.data_dir {
        Some(data) => HostConfig::installed(&exe, data)?,
        None => HostConfig::installed_for_user(&exe)?,
    };
    config.api.port = options.port;
    if let Some(agent) = &options.agent {
        config.guest.agent_binary.clone_from(agent);
    }
    if let Some(file) = &options.connection_file {
        config.api.connection_file = Some(file.clone());
    }
    Ok(config)
}

/// Stops what the backend runs; the report says what the shutdown did.
type Stop = Box<dyn FnOnce() -> Pin<Box<dyn Future<Output = HostShutdown> + Send>> + Send>;

/// Asks a workspace's guest to end its editor's extension hosts (the editor window closed). The
/// error says why it did not reach the guest, for a log: a stopped workspace has none to end.
pub type EndExtensionHosts =
    Arc<dyn Fn(WorkspaceId) -> BoxFuture<'static, Result<(), String>> + Send + Sync>;

/// What the user calls a workspace (its display name, else its name), for a notice; `None` when
/// this backend has no such workspace.
pub type WorkspaceLabel =
    Arc<dyn Fn(WorkspaceId) -> BoxFuture<'static, Option<String>> + Send + Sync>;

/// How many terminals are open now, in every workspace; the quit shortcut asks before it ends them.
pub type OpenTerminals = Arc<dyn Fn() -> usize + Send + Sync>;

/// A running in-process backend.
pub struct Backend {
    info: ConnectionInfo,
    url: Url,
    events: Arc<EventHub>,
    problems: Arc<Problems>,
    store: Arc<Store>,
    settings: Arc<dyn SettingsRepo>,
    extension_hosts: EndExtensionHosts,
    label: WorkspaceLabel,
    open_terminals: OpenTerminals,
    stop: Mutex<Option<Stop>>,
}

impl std::fmt::Debug for Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Backend")
            .field("url", &self.url)
            .finish_non_exhaustive()
    }
}

impl Backend {
    pub(crate) fn new(
        info: ConnectionInfo,
        events: Arc<EventHub>,
        problems: Arc<Problems>,
        store: Arc<Store>,
        settings: Arc<dyn SettingsRepo>,
        stop: Stop,
    ) -> Result<Self, BackendError> {
        Ok(Self {
            url: Url::parse(&info.url)?,
            info,
            events,
            problems,
            store,
            settings,
            extension_hosts: Arc::new(|_| {
                Box::pin(async { Err("this backend runs no workspaces".to_owned()) })
            }),
            label: Arc::new(|_| Box::pin(async { None })),
            open_terminals: Arc::new(|| 0),
            stop: Mutex::new(Some(stop)),
        })
    }

    /// The same backend with `label` as its way to name a workspace in a notice.
    #[must_use]
    pub(crate) fn with_label(mut self, label: WorkspaceLabel) -> Self {
        self.label = label;
        self
    }

    /// The same backend with `count` as its way to count the open terminals.
    #[must_use]
    pub(crate) fn with_open_terminals(mut self, count: OpenTerminals) -> Self {
        self.open_terminals = count;
        self
    }

    /// The same backend with `ends` as its way to end a guest's extension hosts.
    #[must_use]
    pub(crate) fn with_extension_hosts(mut self, ends: EndExtensionHosts) -> Self {
        self.extension_hosts = ends;
        self
    }

    /// The backend over a started host. The host (and with it the data folder's lock) lives until
    /// the shutdown has run.
    fn of_host<R: puddle_compute::Runtime + Clone + 'static>(
        host: Host<R>,
    ) -> Result<Self, BackendError> {
        let (info, events, problems, store, settings) = (
            host.connection().clone(),
            host.events().clone(),
            host.problems().clone(),
            host.store().clone(),
            host.settings().clone(),
        );
        let host = Arc::new(host);
        // Weak: the windows (which hold this handle) live in the host's launcher, and a strong
        // reference would keep the host, and its data folder's lock, alive after the shutdown.
        let weak = Arc::downgrade(&host);
        let ends: EndExtensionHosts = Arc::new(move |workspace| {
            let weak = weak.clone();
            Box::pin(async move {
                let host = weak
                    .upgrade()
                    .ok_or_else(|| "the host has stopped".to_owned())?;
                host.workspaces()
                    .end_extension_hosts(&workspace)
                    .await
                    .map(drop)
                    .map_err(|e| e.to_string())
            })
        });
        let named = Arc::downgrade(&host);
        let label: WorkspaceLabel = Arc::new(move |workspace| {
            let named = named.clone();
            Box::pin(async move {
                let host = named.upgrade()?;
                let record = host.workspaces().get(&workspace).await.ok()?;
                Some(record.label().to_owned())
            })
        });
        let counted = Arc::downgrade(&host);
        let open_terminals: OpenTerminals = Arc::new(move || {
            counted
                .upgrade()
                .map_or(0, |host| host.workspaces().open_terminals())
        });
        Self::new(
            info,
            events,
            problems,
            store,
            settings,
            Box::new(move || Box::pin(async move { host.shutdown().await })),
        )
        .map(|backend| {
            backend
                .with_extension_hosts(ends)
                .with_label(label)
                .with_open_terminals(open_terminals)
        })
    }

    /// The way to ask a workspace's guest to end its editor's extension hosts, for the windows'
    /// last-closed hook to hold.
    #[must_use]
    pub fn extension_hosts(&self) -> EndExtensionHosts {
        Arc::clone(&self.extension_hosts)
    }

    /// The way to ask what the user calls a workspace, for notices.
    #[must_use]
    pub fn label(&self) -> WorkspaceLabel {
        Arc::clone(&self.label)
    }

    /// How many terminals are open now, for the quit shortcut's question. The fixture runs none.
    #[must_use]
    pub fn open_terminals(&self) -> usize {
        (self.open_terminals)()
    }

    /// The API's origin, `http://127.0.0.1:<port>/`: the address of the main window.
    #[must_use]
    pub fn url(&self) -> &Url {
        &self.url
    }

    /// The API token, to hand to the main window's start-up script. Never log it.
    #[must_use]
    pub fn token(&self) -> &str {
        self.info.token.expose()
    }

    /// The event hub: whatever emits events into it reaches the SPA's stream, and Rust code (the
    /// tray and notifications) can subscribe through the API's types.
    #[must_use]
    pub fn events(&self) -> &Arc<EventHub> {
        &self.events
    }

    /// The database: the shell counts the open requests from it for the badge.
    #[must_use]
    pub fn store(&self) -> &Arc<Store> {
        &self.store
    }

    /// The settings documents, which the shell's tray, notifications and close behaviour read.
    #[must_use]
    pub fn settings(&self) -> &Arc<dyn SettingsRepo> {
        &self.settings
    }

    /// The background problems the page lists as notices: where the shell raises what it has to
    /// tell the user (a link that went nowhere).
    #[must_use]
    pub fn problems(&self) -> &Arc<Problems> {
        &self.problems
    }

    /// Stops the backend. The host refuses new workspace operations, lets running ones finish,
    /// trims and stops every workspace's machine, closes the routes, then stops the API and its
    /// background tasks. The report says what ran; `None` when an earlier call already stopped
    /// it.
    pub async fn shutdown(&self) -> Option<HostShutdown> {
        let stop = self.stop.lock().await.take()?;
        Some(stop().await)
    }
}

#[cfg(all(test, feature = "fixture"))]
pub(crate) mod tests;

#[cfg(test)]
mod host_config_tests;
