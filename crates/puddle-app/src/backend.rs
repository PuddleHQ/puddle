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

use puddle_api::{ConnectionFileError, ConnectionInfo, EventHub, ServeError};
use puddle_host::{
    Host, HostConfig, HostError, HostOptions, HostShutdown, MsbFactory, Platform, Prepared,
    RuntimeFactory, prepare,
};
use puddle_types::Problems;
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
    let exe = std::env::current_exe()
        .map_err(|e| HostError::DataDir(format!("cannot find this program's folder: {e}")))?;
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

/// A running in-process backend.
pub struct Backend {
    info: ConnectionInfo,
    url: Url,
    events: Arc<EventHub>,
    problems: Arc<Problems>,
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
        stop: Stop,
    ) -> Result<Self, BackendError> {
        Ok(Self {
            url: Url::parse(&info.url)?,
            info,
            events,
            problems,
            stop: Mutex::new(Some(stop)),
        })
    }

    /// The backend over a started host. The host (and with it the data folder's lock) lives until
    /// the shutdown has run.
    fn of_host<R: puddle_compute::Runtime + Clone + 'static>(
        host: Host<R>,
    ) -> Result<Self, BackendError> {
        let (info, events, problems) = (
            host.connection().clone(),
            host.events().clone(),
            host.problems().clone(),
        );
        let host = Arc::new(host);
        Self::new(
            info,
            events,
            problems,
            Box::new(move || Box::pin(async move { host.shutdown().await })),
        )
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
pub(crate) mod tests {
    #![expect(
        clippy::unwrap_used,
        reason = "helpers outside #[test] fns run only in tests; a panic is how a test fails"
    )]

    use puddle_host::Step;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    use super::*;

    /// One HTTP/1.1 GET over loopback; returns the whole reply as text.
    pub(crate) async fn get(backend: &Backend, path: &str, token: Option<&str>) -> String {
        let addr = (
            backend.url().host_str().unwrap().to_owned(),
            backend.url().port().unwrap(),
        );
        let mut stream = TcpStream::connect(addr.clone()).await.unwrap();
        let auth = token.map_or(String::new(), |t| format!("Authorization: Bearer {t}\r\n"));
        let host = format!("{}:{}", backend.url().host_str().unwrap(), addr.1);
        let request =
            format!("GET {path} HTTP/1.1\r\nHost: {host}\r\n{auth}Connection: close\r\n\r\n");
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut reply = Vec::new();
        stream.read_to_end(&mut reply).await.unwrap();
        String::from_utf8_lossy(&reply).into_owned()
    }

    pub(crate) fn fixture_options() -> Options {
        Options {
            fixture: true,
            ..Options::default()
        }
    }

    pub(crate) async fn start_fixture(port: u16) -> Backend {
        let options = Options {
            port,
            ..fixture_options()
        };
        Source::Fixture.start(&options).await.unwrap()
    }

    #[tokio::test]
    async fn the_fixture_serves_the_api_on_loopback_with_its_token() {
        let backend = start_fixture(0).await;
        assert_eq!(backend.url().host_str(), Some("127.0.0.1"));
        assert_ne!(backend.url().port(), Some(0));
        assert_eq!(backend.token().len(), 64);

        let with = get(&backend, "/api/pending", Some(backend.token())).await;
        assert!(with.starts_with("HTTP/1.1 200"), "{with}");
        assert!(with.contains("registry.example.org"), "{with}");

        let without = get(&backend, "/api/pending", None).await;
        assert!(without.starts_with("HTTP/1.1 401"), "{without}");
        backend.shutdown().await;
    }

    /// The Windows smoke test goes through the first-run flow in the window, so the stand-in
    /// backend has to start as a fresh install; seeding it as set up would hide the flow.
    #[tokio::test]
    async fn the_fixture_is_a_fresh_install_whose_first_run_flow_is_open() {
        let backend = start_fixture(0).await;
        let reply = get(&backend, "/api/first-run", Some(backend.token())).await;
        assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
        assert!(reply.contains("\"completed\":false"), "{reply}");
        backend.shutdown().await;
    }

    #[tokio::test]
    async fn the_fixture_stops_only_its_api_and_a_second_shutdown_does_nothing() {
        let backend = start_fixture(0).await;
        let port = backend.url().port().unwrap();
        assert_eq!(
            backend.shutdown().await,
            Some(HostShutdown {
                steps: vec![Step::ApiStopped],
                ..HostShutdown::default()
            })
        );
        assert!(backend.shutdown().await.is_none());
        assert!(TcpStream::connect(("127.0.0.1", port)).await.is_err());
    }

    #[tokio::test]
    async fn the_fixture_writes_the_connection_file_it_is_asked_for() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("api.json");
        let options = Options {
            connection_file: Some(file.clone()),
            ..fixture_options()
        };
        let backend = Source::Fixture.start(&options).await.unwrap();
        let read = ConnectionInfo::read(&file).unwrap();
        assert_eq!(read.url, backend.url().as_str().trim_end_matches('/'));
        assert_eq!(read.token.expose(), backend.token());
        backend.shutdown().await;
        assert!(format!("{backend:?}").contains("127.0.0.1"));
        assert!(!format!("{backend:?}").contains(backend.token()));
    }

    #[tokio::test]
    async fn two_backends_get_their_own_ports_and_tokens() {
        let a = start_fixture(0).await;
        let b = start_fixture(0).await;
        assert_ne!(a.url(), b.url());
        assert_ne!(a.token(), b.token());
        a.shutdown().await;
        b.shutdown().await;
    }

    #[tokio::test]
    async fn a_taken_port_is_reported() {
        let a = start_fixture(0).await;
        let options = Options {
            port: a.url().port().unwrap(),
            ..fixture_options()
        };
        let err = Source::Fixture.start(&options).await.unwrap_err();
        assert!(matches!(err, BackendError::Serve(_)), "{err:?}");
        a.shutdown().await;
    }
}
