// SPDX-License-Identifier: AGPL-3.0-or-later
//! Binding, serving and shutting down. Connections are served by hyper directly (not
//! `axum::serve`) so headers have a read timeout and every connection task has an owner.

use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::http::{Method, StatusCode, Uri};
use axum::response::IntoResponse;
use hyper::server::conn::http1;
use hyper_util::rt::{TokioIo, TokioTimer};
use hyper_util::service::TowerToHyperService;
use puddle_netpolicy::{
    CompanySignals, EndpointKind, NameRegistration, NoSignals, PuddleEndpoints, Registration,
};
use puddle_secrets::SecretStore;
use puddle_store::{AccountSecrets, Clock, Store};
use puddle_types::Problems;
use tokio::net::TcpListener;
use tokio::sync::{Mutex, watch};
use tokio::task::{JoinHandle, JoinSet};
use tokio_rustls::TlsAcceptor;
use tower_http::timeout::TimeoutLayer;

use crate::admin::AdminService;
use crate::auth::{Guard, Listener, guard};
use crate::authz::{Isolation, Principal, authorize};
use crate::credentials::{CredentialService, NoCredentials, NoSecretStore};
use crate::dev_cert::{DevCertService, NoDevCert};
use crate::doctor::{DoctorService, NoDoctor};
use crate::error::ApiError;
use crate::events::EventHub;
use crate::front::{FrontAddresses, NameResolver, REFRESH_EVERY, SystemResolver};
use crate::learned::{LearnedService, NoLearned};
use crate::network_health::{NetworkHealthService, NoNetworkHealth};
use crate::ports::{NoPorts, PortsService};
use crate::proxy_sign_in::{NoProxySignIns, ProxySignInService};
use crate::remote::{NetworkListener, PublicHost, Remote, RemoteError, RemoteServing};
use crate::repos::{NoRepos, RepoService};
use crate::routes::{AppState, api_router, slow_router};
use crate::settings::SettingsRepo;
use crate::signin::SignIn;
use crate::terminals::{NoTerminals, Registry, TerminalService};
use crate::token::{ApiToken, ConnectionInfo};
use crate::ui::{UiAssets, UiService};
use crate::workspaces::{NoWorkspaces, WorkspaceService};

/// Largest request body (the biggest real one, a settings document, is well under 4 KiB).
const MAX_BODY_BYTES: usize = 64 * 1024;

/// How long the routes of [`slow_router`] may take: the system check's test boot (up to about 30
/// s) and a few checks queued behind each other, or the trust route's wait for the user, which
/// gives up after two minutes on its own.
const SLOW_REQUEST_TIMEOUT: Duration = Duration::from_secs(150);

/// How long a client may take to send a request's headers.
const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(10);

/// How long shutdown waits for open requests before ending them.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// Pause after a failed `accept` (e.g. out of file descriptors) before trying again.
const ACCEPT_BACKOFF: Duration = Duration::from_millis(100);

/// How the API listens. It binds `127.0.0.1`; the only other address is the opt-in
/// [`NetworkListener`] of remote serving, which names its interface and its allowed peers.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ApiConfig {
    /// Port on `127.0.0.1`; 0 (the default) lets the OS pick a free one, which clients learn
    /// from the connection file.
    pub port: u16,
    /// Browser origins besides the API's own that may call it, e.g. the desktop shell's
    /// (`tauri://localhost`). Empty by default. Calls from them still need the token.
    pub extra_origins: Vec<String>,
    /// Longest a non-streaming request may take before it gets a 408.
    pub request_timeout: Duration,
    /// Most connections served at once; further ones are closed at accept.
    pub max_connections: usize,
    /// The single-page app to serve outside `/api`, from the same origin. The default is the
    /// embedded build under feature `embedded-ui` and nothing without it.
    pub ui: Option<Arc<dyn UiAssets>>,
    /// Remote serving: the public names a reverse proxy may use. `None` (the default) keeps
    /// exactly the loopback behaviour.
    ///
    /// Its optional [`NetworkListener`] is the second listener, for a reverse proxy on another
    /// machine.
    pub remote: Option<Remote>,
    /// Whether users other than the owner may sign in. Off by default: everything belongs to the
    /// one owner.
    pub multi_user: bool,
}

impl Default for ApiConfig {
    fn default() -> Self {
        #[cfg(feature = "embedded-ui")]
        let ui: Option<Arc<dyn UiAssets>> = Some(Arc::new(crate::ui::EmbeddedUi));
        #[cfg(not(feature = "embedded-ui"))]
        let ui: Option<Arc<dyn UiAssets>> = None;
        Self {
            port: 0,
            extra_origins: Vec::new(),
            request_timeout: Duration::from_secs(30),
            max_connections: 256,
            ui,
            remote: None,
            multi_user: false,
        }
    }
}

impl ApiConfig {
    /// The default configuration on a fixed port.
    #[must_use]
    pub fn with_port(port: u16) -> Self {
        Self {
            port,
            ..Self::default()
        }
    }
}

/// What the API serves from.
#[derive(Clone)]
#[non_exhaustive]
pub struct Services {
    /// Rules, pending requests and the audit log.
    pub store: Arc<Store>,
    /// Settings documents.
    pub settings: Arc<dyn SettingsRepo>,
    /// The event hub SSE subscribers read from; give the same hub to whatever emits events,
    /// including the store (`Store::with_events`), which emits the pending, rule and audit
    /// events.
    pub events: Arc<EventHub>,
    /// The background problems `GET /api/problems` lists. [`Services::new`] starts with a list
    /// of its own that announces changes on `events`; give the one the host raises into with
    /// [`Services::with_problems`].
    pub problems: Arc<Problems>,
    /// The clock consents are stamped with.
    pub clock: Arc<dyn Clock>,
    /// The workspaces resource. [`Services::new`] starts with [`NoWorkspaces`], which answers
    /// 503; set the real one with [`Services::with_workspaces`].
    pub workspaces: Arc<dyn WorkspaceService>,
    /// The network-health report. [`Services::new`] starts with [`NoNetworkHealth`], which
    /// answers 503; set the real one with [`Services::with_network_health`].
    pub network_health: Arc<dyn NetworkHealthService>,
    /// What the identities screens ask the host: accounts found, credential checks, pasted
    /// tokens, sign-ins. [`Services::new`] starts with [`NoCredentials`], which answers 503; set
    /// the real one with [`Services::with_credentials`].
    pub credentials: Arc<dyn CredentialService>,
    /// Where the values of environment secrets are kept, the operating system's credential store
    /// on a real host. [`Services::new`] starts with one that is never available, so a secret is
    /// refused (503) instead of kept somewhere it would be lost; set the real one with
    /// [`Services::with_secret_store`].
    pub secrets: Arc<dyn SecretStore>,
    /// The repositories the identities' credentials reach, and who a credential's account is.
    /// [`Services::new`] starts with [`NoRepos`], which answers 503; set the real one with
    /// [`Services::with_repos`].
    pub repos: Arc<dyn RepoService>,
    /// The system check. [`Services::new`] starts with [`NoDoctor`], which answers 503; set the
    /// real one with [`Services::with_doctor`].
    pub doctor: Arc<dyn DoctorService>,
    /// Where the development certificate stands. [`Services::new`] starts with [`NoDevCert`],
    /// which says "not checked"; set the real one with [`Services::with_dev_cert`].
    pub dev_cert: Arc<dyn DevCertService>,
    /// A workspace's ports and forwards. [`Services::new`] starts with [`NoPorts`], which answers
    /// 503; set the real one with [`Services::with_ports`].
    pub ports: Arc<dyn PortsService>,
    /// The hosts a workspace's tools refused Puddle's certificate for. [`Services::new`] starts
    /// with [`NoLearned`] (an empty list); set the real one with [`Services::with_learned`].
    pub learned: Arc<dyn LearnedService>,
    /// Terminals: a shell in a running workspace. Starts with [`NoTerminals`]; set the real one with
    /// [`Services::with_terminals`].
    pub terminals: Arc<dyn TerminalService>,
    /// The sign-ins for company proxies that ask for a user name and password.
    /// [`Services::new`] starts with [`NoProxySignIns`] (nothing stored, a change answers 503);
    /// set the real one with [`Services::with_proxy_sign_ins`].
    pub proxy_sign_ins: Arc<dyn ProxySignInService>,
    /// What the host reads of the company network; [`NoSignals`] until the host sets it with
    /// [`Services::with_company_signals`], so no pending request is marked `company_network`.
    pub company_signals: Arc<dyn CompanySignals>,
    /// The registry of Puddle's own listeners that the workspace proxy's guard consults. The API
    /// registers its address here when it binds, so a workspace can't reach it even with the
    /// loopback toggle on. [`Services::new`] starts with an empty registry of its own; give the
    /// one the proxy uses with [`Services::with_endpoints`].
    pub endpoints: PuddleEndpoints,
    /// The sign-in secrets browsers sign in against (`Store::take_account_secrets`). Without
    /// them, remote serving has no sign-in: the routes answer 503 and no browser is let in.
    pub account_secrets: Option<Arc<AccountSecrets>>,
    /// How the front's names are resolved, to block the addresses they point at.
    /// [`Services::new`] starts with the system's resolver; set another with
    /// [`Services::with_front_resolver`].
    pub front_resolver: Arc<dyn NameResolver>,
}

impl Services {
    /// Services over these parts.
    #[must_use]
    pub fn new(
        store: Arc<Store>,
        settings: Arc<dyn SettingsRepo>,
        events: Arc<EventHub>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            store,
            settings,
            problems: Arc::new(Problems::new(events.clone())),
            events,
            clock,
            workspaces: Arc::new(NoWorkspaces),
            network_health: Arc::new(NoNetworkHealth),
            credentials: Arc::new(NoCredentials),
            secrets: Arc::new(NoSecretStore),
            repos: Arc::new(NoRepos),
            doctor: Arc::new(NoDoctor),
            dev_cert: Arc::new(NoDevCert),
            ports: Arc::new(NoPorts),
            learned: Arc::new(NoLearned),
            terminals: Arc::new(NoTerminals),
            proxy_sign_ins: Arc::new(NoProxySignIns),
            company_signals: Arc::new(NoSignals),
            endpoints: PuddleEndpoints::new(),
            account_secrets: None,
            front_resolver: Arc::new(SystemResolver),
        }
    }

    /// These services resolving the front's names with `resolver`.
    #[must_use]
    pub fn with_front_resolver(mut self, resolver: Arc<dyn NameResolver>) -> Self {
        self.front_resolver = resolver;
        self
    }

    /// These services with browser sign-in over `secrets`.
    #[must_use]
    pub fn with_account_secrets(mut self, secrets: AccountSecrets) -> Self {
        self.account_secrets = Some(Arc::new(secrets));
        self
    }

    /// These services listing the problems raised into `problems`.
    #[must_use]
    pub fn with_problems(mut self, problems: Arc<Problems>) -> Self {
        self.problems = problems;
        self
    }

    /// These services with this network-health report.
    #[must_use]
    pub fn with_network_health(mut self, network_health: Arc<dyn NetworkHealthService>) -> Self {
        self.network_health = network_health;
        self
    }

    /// These services with this credentials implementation.
    #[must_use]
    pub fn with_credentials(mut self, credentials: Arc<dyn CredentialService>) -> Self {
        self.credentials = credentials;
        self
    }

    /// These services keeping the values of environment secrets in `secrets`.
    #[must_use]
    pub fn with_secret_store(mut self, secrets: Arc<dyn SecretStore>) -> Self {
        self.secrets = secrets;
        self
    }

    /// These services with this repository lists implementation.
    #[must_use]
    pub fn with_repos(mut self, repos: Arc<dyn RepoService>) -> Self {
        self.repos = repos;
        self
    }

    /// These services reporting the development certificate's state through `dev_cert`.
    #[must_use]
    pub fn with_dev_cert(mut self, dev_cert: Arc<dyn DevCertService>) -> Self {
        self.dev_cert = dev_cert;
        self
    }

    /// These services with this system check.
    #[must_use]
    pub fn with_doctor(mut self, doctor: Arc<dyn DoctorService>) -> Self {
        self.doctor = doctor;
        self
    }

    /// These services with this ports implementation.
    #[must_use]
    pub fn with_ports(mut self, ports: Arc<dyn PortsService>) -> Self {
        self.ports = ports;
        self
    }

    /// These services with this learned-hosts implementation.
    #[must_use]
    pub fn with_learned(mut self, learned: Arc<dyn LearnedService>) -> Self {
        self.learned = learned;
        self
    }

    /// These services with the terminals service.
    #[must_use]
    pub fn with_terminals(mut self, terminals: Arc<dyn TerminalService>) -> Self {
        self.terminals = terminals;
        self
    }

    /// These services with this proxy sign-in implementation.
    #[must_use]
    pub fn with_proxy_sign_ins(mut self, proxy_sign_ins: Arc<dyn ProxySignInService>) -> Self {
        self.proxy_sign_ins = proxy_sign_ins;
        self
    }

    /// These services reading the company network through `signals`, the same the workspace
    /// proxy's guard reads, so a pending request is marked `company_network` when the guard would
    /// refuse it for that reason.
    #[must_use]
    pub fn with_company_signals(mut self, signals: Arc<dyn CompanySignals>) -> Self {
        self.company_signals = signals;
        self
    }

    /// These services registering the API in `endpoints`, the registry the workspace proxy's guard
    /// uses.
    #[must_use]
    pub fn with_endpoints(mut self, endpoints: PuddleEndpoints) -> Self {
        self.endpoints = endpoints;
        self
    }

    /// These services with this workspaces implementation.
    #[must_use]
    pub fn with_workspaces(mut self, workspaces: Arc<dyn WorkspaceService>) -> Self {
        self.workspaces = workspaces;
        self
    }
}

/// Why the API couldn't start.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ServeError {
    /// The port couldn't be bound.
    #[error("cannot listen on 127.0.0.1:{port}: {source}")]
    Bind {
        /// The requested port.
        port: u16,
        /// What failed.
        source: io::Error,
    },
    /// The network listener couldn't be bound.
    #[error("cannot listen on {addr} for a reverse proxy: {source}")]
    BindNetwork {
        /// The interface and port.
        addr: SocketAddr,
        /// What failed.
        source: io::Error,
    },
    /// The remote-serving settings do not fit together.
    #[error("{0}")]
    Remote(#[from] RemoteError),
    /// The listener's certificate or key cannot be used.
    #[error("the network listener's TLS certificate cannot be used: {0}")]
    Tls(String),
    /// Sign-in could not start (the system gave no randomness).
    #[error("browser sign-in cannot start: {0}")]
    SignIn(String),
}

/// A bound, not yet serving API.
pub struct ApiServer {
    listener: TcpListener,
    addr: SocketAddr,
    router: Router,
    token: ApiToken,
    max_connections: usize,
    stop: watch::Sender<bool>,
    registration: Registration,
    remote: Option<RemoteBound>,
    /// Finishes the deletions the owner started.
    admin: Option<Arc<AdminService>>,
}

/// What remote serving binds and registers.
struct RemoteBound {
    network: Option<NetworkBound>,
    /// The external front's ports on every own address.
    fronts: Vec<Registration>,
    /// The public names, blocked for guests.
    names: Vec<NameRegistration>,
    /// The addresses of a front on another machine, blocked for guests.
    addresses: Arc<FrontAddresses>,
}

/// The bound network listener and how it admits peers.
struct NetworkBound {
    bound: BoundNetwork,
    router: Router,
}

/// The network listener before its routes exist.
struct BoundNetwork {
    listener: TcpListener,
    addr: SocketAddr,
    admission: Admission,
    registration: Registration,
}

/// Who may connect to a listener, decided at accept.
#[derive(Clone)]
enum Admission {
    /// Loopback peers only.
    Loopback,
    /// The listed proxy addresses only, then TLS.
    Proxies {
        allowed: Vec<IpAddr>,
        tls: TlsAcceptor,
    },
}

impl Admission {
    pub(crate) fn admits(&self, peer: IpAddr) -> bool {
        match self {
            Self::Loopback => peer.is_loopback(),
            Self::Proxies { allowed, .. } => {
                let peer = peer.to_canonical();
                allowed.iter().any(|a| a.to_canonical() == peer)
            }
        }
    }
}

/// The TLS acceptor for the network listener: TLS 1.2 and 1.3, HTTP/1.1, no client certificate
/// (who may connect is decided by address, before the handshake).
fn tls_acceptor(identity: &crate::remote::TlsIdentity) -> Result<TlsAcceptor, ServeError> {
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
    let fail = |e: rustls::Error| ServeError::Tls(e.to_string());
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(identity.key_pkcs8_der().to_vec()));
    let mut config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(fail)?
    .with_no_client_auth()
    .with_single_cert(
        vec![CertificateDer::from(identity.cert_der().to_vec())],
        key,
    )
    .map_err(fail)?;
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(TlsAcceptor::from(Arc::new(config)))
}

/// Binds the network listener and registers its address, before anything serves.
async fn bind_network(
    network: &NetworkListener,
    endpoints: &PuddleEndpoints,
) -> Result<BoundNetwork, ServeError> {
    let endpoint = network.endpoint();
    let requested = SocketAddr::new(endpoint.interface(), endpoint.port());
    let tls = tls_acceptor(network.tls())?;
    let bound = async {
        let listener = TcpListener::bind(requested).await?;
        let addr = listener.local_addr()?;
        Ok::<_, io::Error>((listener, addr))
    };
    let (listener, addr) = bound.await.map_err(|source| ServeError::BindNetwork {
        addr: requested,
        source,
    })?;
    let registration = endpoints.register(addr, EndpointKind::RemoteApi);
    tracing::info!(%addr, "api listening for a reverse proxy (TLS)");
    Ok(BoundNetwork {
        listener,
        addr,
        admission: Admission::Proxies {
            allowed: endpoint.allowed_proxies().to_vec(),
            tls,
        },
        registration,
    })
}

/// The external front's ports on every own address: a guest dialing any of this host's
/// addresses on a port the front uses is refused, whatever the toggles and rules say.
fn front_registrations(remote: &RemoteServing, endpoints: &PuddleEndpoints) -> Vec<Registration> {
    let mut ports: Vec<u16> = remote.public_hosts().iter().map(PublicHost::port).collect();
    ports.sort_unstable();
    ports.dedup();
    ports
        .into_iter()
        .map(|port| {
            let any = SocketAddr::from((Ipv4Addr::UNSPECIFIED, port));
            endpoints.register(any, EndpointKind::ExternalFront)
        })
        .collect()
}

/// The public names and the workspace base domain, blocked by name: a front on another machine
/// has no address of this host, so only its name says what it is.
fn name_blocks(remote: &RemoteServing, endpoints: &PuddleEndpoints) -> Vec<NameRegistration> {
    let mut blocks: Vec<NameRegistration> = remote
        .public_hosts()
        .iter()
        .map(|h| endpoints.block_name(h.host(), false, EndpointKind::ExternalFront))
        .collect();
    let base = remote.workspace_names().base_domain();
    blocks.push(endpoints.block_name(base, true, EndpointKind::ExternalFront));
    blocks
}

/// Everything remote serving blocks for guests: the front's ports, its names and its addresses.
async fn front_blocks(
    remote: &Remote,
    services: &Services,
) -> (
    Vec<Registration>,
    Vec<NameRegistration>,
    Arc<FrontAddresses>,
) {
    let addresses = Arc::new(FrontAddresses::new(
        remote,
        &services.endpoints,
        services.front_resolver.clone(),
    ));
    // The first lookup is awaited, so the addresses are blocked before anything serves; a name
    // that does not answer in time is retried by the refresh task.
    addresses.refresh().await;
    (
        front_registrations(remote.serving(), &services.endpoints),
        name_blocks(remote.serving(), &services.endpoints),
        addresses,
    )
}

/// Resolves the front's names again every [`REFRESH_EVERY`] until the API stops.
async fn refresh_front_addresses(addresses: Arc<FrontAddresses>, mut stop: watch::Receiver<bool>) {
    let mut tick = tokio::time::interval(REFRESH_EVERY);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    tick.tick().await; // the first tick is now, and bind already looked
    loop {
        tokio::select! {
            _ = tick.tick() => addresses.refresh().await,
            () = async { let _ = stop.wait_for(|stopped| *stopped).await; } => return,
        }
    }
}

/// Binds the network listener when remote serving has one.
async fn bind_network_for(
    config: &ApiConfig,
    endpoints: &PuddleEndpoints,
) -> Result<Option<BoundNetwork>, ServeError> {
    match config.remote.as_ref().and_then(Remote::network_listener) {
        Some(network) => Ok(Some(bind_network(network, endpoints).await?)),
        None => Ok(None),
    }
}

/// Browser sign-in, when remote serving is on and the host gave the API its sign-in secrets.
async fn sign_in_for(
    config: &ApiConfig,
    services: &Services,
) -> Result<Option<Arc<SignIn>>, ServeError> {
    let (Some(_), Some(secrets)) = (&config.remote, services.account_secrets.clone()) else {
        return Ok(None);
    };
    let sign_in = Arc::new(
        SignIn::new(
            services.store.clone(),
            secrets,
            services.clock.clone(),
            config.multi_user,
        )
        .map_err(|e| ServeError::SignIn(e.reason().to_owned()))?,
    );
    warn_while_unclaimed(&sign_in).await;
    Ok(Some(sign_in))
}

/// The owner's admin screen: only with several users on, and only where there is sign-in.
fn admin_for(
    config: &ApiConfig,
    sign_in: Option<&Arc<SignIn>>,
    services: &Services,
) -> Option<Arc<AdminService>> {
    let sign_in = sign_in.filter(|_| config.multi_user)?;
    Some(Arc::new(AdminService::new(
        services.store.clone(),
        sign_in.clone(),
        services.workspaces.clone(),
    )))
}

/// Says so in the log while nobody has made the owner account: the first visitor to the public
/// address would.
async fn warn_while_unclaimed(sign_in: &Arc<SignIn>) {
    if sign_in.status().await.is_ok_and(|status| status.setup_open) {
        tracing::warn!(
            "no owner account exists yet: the first visitor to the public address will create it"
        );
    }
}

impl ApiServer {
    /// Binds `127.0.0.1:<config.port>` and builds the routes.
    ///
    /// # Errors
    ///
    /// [`ServeError::Bind`] if the port is taken or can't be bound.
    pub async fn bind(
        config: ApiConfig,
        token: ApiToken,
        services: Services,
    ) -> Result<Self, ServeError> {
        Self::bind_as(config, token, services, Principal::INSTALL).await
    }

    /// [`ApiServer::bind`] with the token standing for `principal`.
    pub(crate) async fn bind_as(
        config: ApiConfig,
        token: ApiToken,
        services: Services,
        principal: Principal,
    ) -> Result<Self, ServeError> {
        let bind_err = |source| ServeError::Bind {
            port: config.port,
            source,
        };
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, config.port))
            .await
            .map_err(bind_err)?;
        let addr = listener.local_addr().map_err(bind_err)?;
        // Registered before anything can serve, so there is no moment a guest could reach it.
        let registration = services.endpoints.register(addr, EndpointKind::Api);
        let network = bind_network_for(&config, &services.endpoints).await?;
        let fronts_names = match config.remote.as_ref() {
            Some(remote) => Some(front_blocks(remote, &services).await),
            None => None,
        };
        let (stop, shutdown) = watch::channel(false);
        let settings_lock = services
            .workspaces
            .settings_lock()
            .unwrap_or_else(|| Arc::new(Mutex::new(())));
        let sign_in = sign_in_for(&config, &services).await?;
        let admin = admin_for(&config, sign_in.as_ref(), &services);
        let state = AppState {
            admin: admin.clone(),
            authorizer: Arc::new(Isolation::new(services.store.clone())),
            store: services.store,
            settings: services.settings,
            settings_lock,
            events: services.events,
            problems: services.problems,
            clock: services.clock,
            workspaces: services.workspaces,
            network_health: services.network_health,
            credentials: services.credentials,
            secrets: services.secrets,
            repos: services.repos,
            doctor: services.doctor,
            dev_cert: services.dev_cert,
            ports: services.ports,
            learned: services.learned,
            terminal_service: services.terminals,
            terminals: Arc::new(Registry::default()),
            proxy_sign_ins: services.proxy_sign_ins,
            company_signals: services.company_signals,
            sign_in: sign_in.clone(),
            ui_build_id: config.ui.as_deref().and_then(crate::ui::build_id),
            shutdown,
        };
        // System managed follows the stored setup from the first request on (rules spec R-41).
        crate::system_managed::refresh(&state).await;
        let ui = config
            .ui
            .clone()
            .map(|assets| UiService::new(assets).with_opener_policy(config.remote.is_some()));
        let guard_for = |listener| {
            let guard = Guard::new(token.clone(), addr.port(), &config.extra_origins)
                .serving_ui(ui.is_some())
                .acting_as(principal);
            let guard = sign_in
                .iter()
                .fold(guard, |guard, sign_in| guard.signing_in(sign_in.clone()));
            config.remote.iter().fold(guard, |guard, remote| {
                guard.remote(remote.serving(), listener)
            })
        };
        let router = build_router(
            state.clone(),
            guard_for(Listener::Loopback),
            config.request_timeout,
            ui.clone(),
        );
        let remote = fronts_names.map(|(fronts, names, addresses)| RemoteBound {
            network: network.map(|bound| NetworkBound {
                router: build_router(
                    state.clone(),
                    guard_for(Listener::Network),
                    config.request_timeout,
                    ui.clone(),
                ),
                bound,
            }),
            fronts,
            names,
            addresses,
        });
        tracing::info!(%addr, "api listening");
        Ok(Self {
            listener,
            addr,
            router,
            token,
            max_connections: config.max_connections.max(1),
            stop,
            registration,
            remote,
            admin,
        })
    }

    /// The bound address (always `127.0.0.1`).
    #[must_use]
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    /// The network listener's bound address, when remote serving has one.
    #[must_use]
    pub fn network_addr(&self) -> Option<SocketAddr> {
        self.remote
            .as_ref()?
            .network
            .as_ref()
            .map(|network| network.bound.addr)
    }

    /// What a client needs to connect, for [`ConnectionInfo::write`].
    #[must_use]
    pub fn connection_info(&self) -> ConnectionInfo {
        ConnectionInfo {
            url: format!("http://{}", self.addr),
            token: self.token.clone(),
        }
    }

    /// Starts serving on a task owned by the returned handle.
    #[must_use]
    pub fn spawn(self) -> RunningApi {
        let Self {
            listener,
            addr,
            router,
            max_connections,
            stop,
            registration,
            remote,
            admin,
            ..
        } = self;
        let shutdown = stop.subscribe();
        let mut tasks = vec![tokio::spawn(serve(
            listener,
            Admission::Loopback,
            router,
            max_connections,
            shutdown.clone(),
        ))];
        if let Some(admin) = admin {
            tasks.push(tokio::spawn(admin.run_reaper(shutdown.clone())));
        }
        let mut network_addr = None;
        let mut held: Vec<Box<dyn std::any::Any + Send>> = vec![Box::new(registration)];
        if let Some(RemoteBound {
            network,
            fronts,
            names,
            addresses,
        }) = remote
        {
            held.push(Box::new(fronts));
            held.push(Box::new(names));
            tasks.push(tokio::spawn(refresh_front_addresses(
                addresses.clone(),
                shutdown.clone(),
            )));
            held.push(Box::new(addresses));
            if let Some(NetworkBound { bound, router }) = network {
                let BoundNetwork {
                    listener,
                    addr,
                    admission,
                    registration,
                } = bound;
                network_addr = Some(addr);
                held.push(Box::new(registration));
                tasks.push(tokio::spawn(serve(
                    listener,
                    admission,
                    router,
                    max_connections,
                    shutdown,
                )));
            }
        }
        RunningApi {
            addr,
            network_addr,
            stop,
            tasks,
            _held: held,
        }
    }
}

/// A serving API. Dropping it without [`RunningApi::shutdown`] aborts the server task.
pub struct RunningApi {
    addr: SocketAddr,
    network_addr: Option<SocketAddr>,
    stop: watch::Sender<bool>,
    tasks: Vec<JoinHandle<()>>,
    /// Keeps the listeners and the blocked names in the endpoint registry for as long as the API
    /// serves.
    _held: Vec<Box<dyn std::any::Any + Send>>,
}

impl RunningApi {
    /// The bound address.
    #[must_use]
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    /// The network listener's bound address, when remote serving has one.
    #[must_use]
    pub fn network_addr(&self) -> Option<SocketAddr> {
        self.network_addr
    }

    /// Stops accepting, ends event streams, lets open requests finish (up to 5 s) and returns
    /// when every connection is closed.
    pub async fn shutdown(mut self) {
        // Receivers live in the server task; if it already ended there is nobody to tell.
        let _ = self.stop.send(true);
        for task in &mut self.tasks {
            if let Err(err) = task.await {
                tracing::error!(error = %err, "api server task failed");
            }
        }
    }
}

impl Drop for RunningApi {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

fn build_router(
    state: AppState,
    guard_state: Guard,
    request_timeout: Duration,
    ui: Option<UiService>,
) -> Router {
    let (router, _spec) = api_router().split_for_parts();
    let (slow, _spec) = slow_router().split_for_parts();
    // Every route, and only routes (not the app's files), goes through its declared check.
    let checked = axum::middleware::from_fn_with_state(state.clone(), authorize);
    router
        .route(
            "/api/openapi.json",
            axum::routing::get(|| async {
                (
                    [(axum::http::header::CONTENT_TYPE, "application/json")],
                    crate::openapi_json(),
                )
            }),
        )
        .route_layer(checked.clone())
        .fallback(move |method: Method, uri: Uri| {
            let ui = ui.clone();
            async move {
                match ui {
                    Some(ui) if !crate::auth::is_api_path(uri.path()) => {
                        ui.respond(&method, uri.path())
                    }
                    _ => ApiError::not_found("no such route").into_response(),
                }
            }
        })
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            request_timeout,
        ))
        .merge(
            slow.route_layer(checked)
                .layer(TimeoutLayer::with_status_code(
                    StatusCode::REQUEST_TIMEOUT,
                    SLOW_REQUEST_TIMEOUT,
                )),
        )
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(axum::middleware::from_fn_with_state(guard_state, guard))
        .with_state(state)
}

async fn serve(
    listener: TcpListener,
    admission: Admission,
    router: Router,
    max_connections: usize,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut connections = JoinSet::new();
    loop {
        // Reap finished connections so the cap counts open ones only.
        while connections.try_join_next().is_some() {}
        let accepted = tokio::select! {
            _ = shutdown.wait_for(|stop| *stop) => break,
            accepted = listener.accept() => accepted,
        };
        let (stream, peer) = match accepted {
            Ok(accepted) => accepted,
            Err(err) => {
                tracing::warn!(error = %err, "api accept failed");
                tokio::time::sleep(ACCEPT_BACKOFF).await;
                continue;
            }
        };
        // Before any bytes are read, and for the network listener before the TLS handshake.
        if !admission.admits(peer.ip()) || connections.len() >= max_connections {
            tracing::warn!(open = connections.len(), "api connection refused");
            drop(stream);
            continue;
        }
        let router = router.clone();
        let stop = shutdown.clone();
        match &admission {
            Admission::Loopback => {
                connections.spawn(serve_connection(stream, router, stop));
            }
            Admission::Proxies { tls, .. } => {
                let tls = tls.clone();
                connections.spawn(async move {
                    match tokio::time::timeout(HEADER_READ_TIMEOUT, tls.accept(stream)).await {
                        Ok(Ok(stream)) => serve_connection(stream, router, stop).await,
                        Ok(Err(err)) => {
                            tracing::debug!(error = %err, "api TLS handshake failed");
                        }
                        Err(_) => tracing::debug!("api TLS handshake timed out"),
                    }
                });
            }
        }
    }
    drop(listener);
    let drained = async { while connections.join_next().await.is_some() {} };
    if tokio::time::timeout(SHUTDOWN_GRACE, drained).await.is_err() {
        tracing::warn!("api connections still open after the grace period; closing them");
    }
    connections.shutdown().await;
    tracing::info!("api stopped");
}

/// Serves one connection until it ends or shutdown asks it to finish.
async fn serve_connection<S>(stream: S, router: Router, mut stop: watch::Receiver<bool>)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let service = TowerToHyperService::new(router);
    // `with_upgrades` lets a terminal's WebSocket take the connection over.
    let mut connection = Box::pin(
        http1::Builder::new()
            .timer(TokioTimer::new())
            .header_read_timeout(HEADER_READ_TIMEOUT)
            .serve_connection(TokioIo::new(stream), service)
            .with_upgrades(),
    );
    let ended = tokio::select! {
        ended = connection.as_mut() => ended,
        () = async { let _ = stop.wait_for(|stop| *stop).await; } => {
            // Finishes the request in flight, then closes.
            connection.as_mut().graceful_shutdown();
            connection.await
        }
    };
    if let Err(err) = ended {
        tracing::debug!(error = %err, "api connection ended with an error");
    }
}

#[cfg(test)]
mod tests;
