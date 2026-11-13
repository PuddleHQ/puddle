// SPDX-License-Identifier: GPL-3.0-or-later
//! One proxied guest connection: read the request head, decide, connect, splice.
//!
//! The order matters for security: the head is bounded in size and time before anything else;
//! the rules engine decides on the normalised name **before** it is resolved (R-10); every
//! resolved address is checked against its IP rules (R-27) and its address class (R-14), and the
//! proxy connects only to an address that passed.
//! Every refusal is an HTTP error response followed by a clean close (a reset could overtake the
//! response); every failure after the connection is open is passed on as a reset.

use std::io;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use puddle_agent_proto::forward::GuestLink;
use puddle_agent_proto::host::{
    AgentReport, AgentReports, AgentStream, GuestStream, GuestWatcher, HostConfig, StreamHandler,
    UdpRefusal,
};
use puddle_agent_proto::relay::splice;
use puddle_agent_proto::resolve::{ResolveAnswer, ResolveQuery};
use puddle_types::{
    BlockReason, ConnectionDecision, ConnectionEvent, ConnectionLog, ConnectionOutcome,
    ConnectionReason, Decision, EgressRequest, EventSink, Host, HttpRequestLine, NullConnectionLog,
    PatternKind, PendingOutcome, Policy, PolicyError, ProtocolHint, RefusedReason, RuleId,
    SuffixAllows, WorkspaceId,
};
use tokio::io::{AsyncBufRead, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::Semaphore;
use tracing::Instrument;

use puddle_netpolicy::{
    LocalAccess, LocalCategory, NetPolicy, block_message, company_network_message, normalise_host,
};

use crate::coalesce::Coalescing;
use crate::counted::Counted;
use crate::destination::{AddressCheck, AddressVerdict, Resolver, SystemResolver};
use crate::http::{self, Body, Head, HeadError, RawTarget};
use crate::opened::{ESTABLISHED, Opened};
use crate::plain;
use crate::records::{RecordResolver, SystemRecords};
use crate::ssh;
use crate::tap::RequestTap;
use crate::target::Target;
use crate::terminate::{self, TerminationSource};
use crate::token_gate::{self, TokenGate};
use crate::upstream::{Admitted, ProxyForm, Upstream, connect_out};
use puddle_upstream::RouteWatch;

/// Limits and timeouts. The defaults suit a real guest; tests shorten them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct ProxyConfig {
    /// How long a guest may take to send its whole request head (slow-loris guard). Over it:
    /// `408`.
    pub head_timeout: Duration,
    /// How long resolving an allowed name may take. Over it: `504`.
    pub resolve_timeout: Duration,
    /// How long connecting to one address may take before the next is tried. All failed: `502`.
    pub connect_timeout: Duration,
    /// Proxied connections one workspace may have open at once. Over it: `503`. Far above what
    /// real tools open (pnpm, `BuildKit` and `NuGet` peak in the hundreds).
    pub max_streams_per_workspace: usize,
    /// Agent connections (yamux sessions) one route accepts at once. The agent opens 1 to 64.
    pub max_sessions_per_route: usize,
    /// Per-session limits of the agent protocol.
    pub session: HostConfig,
    /// How long the proxy's own TLS handshake with a bound host may take.
    pub tls_handshake_timeout: Duration,
    /// How long each step of the guest's TLS handshake with the proxy may take: its hello after
    /// the `CONNECT` answer, then the rest of the handshake. The proxy plays the server here, so
    /// the guest gets what common servers give a client by default (nginx's
    /// `ssl_handshake_timeout`, Python's asyncio: 60 s): a client stalled for longer than the
    /// upstream budget, by a busy disk or a paused machine, still connects instead of finding its
    /// connection cut.
    pub guest_handshake_timeout: Duration,
    /// How long a terminated connection, or a guest connection carrying plain HTTP, may sit idle
    /// between two requests before it is closed.
    pub keepalive_timeout: Duration,
    /// How long a bound host may take to answer once the whole request is sent. Over it: `504`.
    pub upstream_head_timeout: Duration,
    /// How long either side of a terminated HTTP/1.1 request may stall in the middle of a body
    /// before the connection is dropped. HTTP/2 streams are kept alive by pings instead.
    pub body_idle_timeout: Duration,
    /// How long one name lookup for the guest's stub DNS may take. Over it, the stub gets
    /// `SERVFAIL` (or, with a company proxy in the route, a stand-in address).
    pub lookup_timeout: Duration,
    /// Name lookups (of allowed names) one workspace may run at once. A lookup over it waits up
    /// to `lookup_timeout` for a free slot; after that the stub gets `SERVFAIL`.
    pub max_lookups_per_workspace: usize,
    /// HTTP/2 streams one workspace may have open at once across its terminated connections.
    /// A stream over it gets a `503`. Each open stream can hold up to a flow-control window of
    /// the guest's data, so this is also the ceiling of the memory hostile guests can pin.
    pub max_h2_streams_per_workspace: usize,
    /// How long an HTTP/2 stream of a terminated connection may move no frame in either direction
    /// before it is reset. A watch or event stream is quiet for long stretches, so this is far
    /// above the body-stall timeout of HTTP/1.1; the connection's ping catches a dead peer sooner.
    pub stream_idle_timeout: Duration,
    /// How long an agreed WebSocket of a terminated connection may move no byte in either
    /// direction before it is closed.
    pub websocket_idle_timeout: Duration,
    /// How long a terminated HTTP/2 connection may take to end after the proxy asked it to shut
    /// down (idle too long, or a stream that would not give up) before it is dropped.
    pub shutdown_grace: Duration,
    /// How many plain-HTTP requests one guest connection may carry. The answer to the last says
    /// `connection: close`, and the guest's client opens a new connection for the next. Each
    /// request is decided and recorded on its own either way; the limit bounds how long one
    /// connection lives. The idle time between two requests is `keepalive_timeout`.
    pub max_requests_per_connection: usize,
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            head_timeout: Duration::from_secs(30),
            resolve_timeout: Duration::from_secs(10),
            connect_timeout: Duration::from_secs(10),
            max_streams_per_workspace: 4096,
            max_sessions_per_route: 128,
            session: HostConfig::default(),
            tls_handshake_timeout: Duration::from_secs(10),
            guest_handshake_timeout: Duration::from_secs(60),
            keepalive_timeout: Duration::from_secs(60),
            upstream_head_timeout: Duration::from_secs(300),
            body_idle_timeout: Duration::from_secs(120),
            lookup_timeout: Duration::from_secs(3),
            max_lookups_per_workspace: 128,
            max_h2_streams_per_workspace: 2048,
            stream_idle_timeout: Duration::from_secs(3600),
            websocket_idle_timeout: Duration::from_secs(3600),
            shutdown_grace: Duration::from_secs(20),
            max_requests_per_connection: 1000,
        }
    }
}

impl ProxyConfig {
    /// The default limits with another name-lookup timeout and per-workspace lookup cap.
    #[must_use]
    pub fn with_lookup_limits(mut self, timeout: Duration, max_per_workspace: usize) -> Self {
        self.lookup_timeout = timeout;
        self.max_lookups_per_workspace = max_per_workspace;
        self
    }

    /// The default limits with another per-workspace HTTP/2 stream budget.
    #[must_use]
    pub fn with_max_h2_streams_per_workspace(mut self, max: usize) -> Self {
        self.max_h2_streams_per_workspace = max;
        self
    }

    /// The default limits with other idle limits for HTTP/2 streams and agreed `WebSocket`s, and
    /// another grace for a connection that was asked to shut down.
    #[must_use]
    pub fn with_idle_limits(
        mut self,
        stream: Duration,
        websocket: Duration,
        shutdown_grace: Duration,
    ) -> Self {
        self.stream_idle_timeout = stream;
        self.websocket_idle_timeout = websocket;
        self.shutdown_grace = shutdown_grace;
        self
    }

    /// The default limits with another head timeout.
    #[must_use]
    pub fn with_head_timeout(mut self, timeout: Duration) -> Self {
        self.head_timeout = timeout;
        self
    }

    /// The default limits with another per-workspace stream cap.
    #[must_use]
    pub fn with_max_streams_per_workspace(mut self, max: usize) -> Self {
        self.max_streams_per_workspace = max;
        self
    }

    /// The default limits with another per-route session cap.
    #[must_use]
    pub fn with_max_sessions_per_route(mut self, max: usize) -> Self {
        self.max_sessions_per_route = max;
        self
    }

    /// The default limits with other resolve and connect timeouts.
    #[must_use]
    pub fn with_network_timeouts(mut self, resolve: Duration, connect: Duration) -> Self {
        self.resolve_timeout = resolve;
        self.connect_timeout = connect;
        self
    }

    /// The default limits with other timeouts for terminated connections: TLS handshake (both
    /// sides), idle between requests, wait for an upstream answer, stall inside a body.
    #[must_use]
    pub fn with_terminate_timeouts(
        mut self,
        handshake: Duration,
        keepalive: Duration,
        upstream_head: Duration,
        body_idle: Duration,
    ) -> Self {
        self.tls_handshake_timeout = handshake;
        self.guest_handshake_timeout = handshake;
        self.keepalive_timeout = keepalive;
        self.upstream_head_timeout = upstream_head;
        self.body_idle_timeout = body_idle;
        self
    }

    /// The default limits with another budget for the guest's TLS handshake only.
    #[must_use]
    pub fn with_guest_handshake_timeout(mut self, timeout: Duration) -> Self {
        self.guest_handshake_timeout = timeout;
        self
    }

    /// The default limits with another number of plain-HTTP requests per guest connection.
    #[must_use]
    pub fn with_max_requests_per_connection(mut self, max: usize) -> Self {
        self.max_requests_per_connection = max.max(1);
        self
    }

    /// The default limits with other agent-session limits.
    #[must_use]
    pub fn with_session(mut self, session: HostConfig) -> Self {
        self.session = session;
        self
    }
}

/// The egress proxy: one per Puddle process, shared by every workspace's route.
///
/// ```
/// # use std::sync::Arc;
/// # use puddle_proxy::Proxy;
/// # use puddle_types::{Decision, EgressRequest, NullSink, Policy, PolicyError, SuffixAllows};
/// # struct Store;
/// # impl Policy for Store {
/// #     fn decide(&self, _: &EgressRequest, _: SuffixAllows) -> Result<Decision, PolicyError> {
/// #         Err(PolicyError { reason: "demo".into() })
/// #     }
/// # }
/// let proxy = Arc::new(Proxy::new(Arc::new(Store), Arc::new(NullSink)));
/// # let _ = proxy;
/// ```
pub struct Proxy {
    policy: Arc<dyn Policy>,
    resolver: Arc<dyn Resolver>,
    records: Arc<dyn RecordResolver>,
    addresses: Arc<dyn AddressCheck>,
    log: Arc<dyn ConnectionLog>,
    upstream: Option<Upstream>,
    terminations: Option<TerminationConfig>,
    git_ssh: Option<crate::ssh_git::GitSsh>,
    ssh_agents: Option<Arc<crate::ssh_agent::SshAgents>>,
    guest: Option<Arc<dyn GuestWatcher>>,
    pki: Arc<puddle_netpolicy::PkiDistributionPoints>,
    port_probe: Option<Arc<dyn crate::probe::PortProbe>>,
    pub(crate) sink: Arc<dyn EventSink>,
    reports: Option<Arc<dyn AgentReports>>,
    pub(crate) config: ProxyConfig,
    /// How many upstream connections have ended; tests wait on it (feature `testing`).
    #[cfg(feature = "testing")]
    pub(crate) upstream_ended: Arc<std::sync::atomic::AtomicU64>,
}

impl std::fmt::Debug for Proxy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Proxy")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl Proxy {
    /// A proxy that asks `policy`, resolves with the OS resolver (concurrent lookups of one name
    /// are shared), and reports agent events (OOM kills) to `sink`. Its address check is a [`NetPolicy`] with every local toggle off and no
    /// registered endpoints: public addresses only. The host program passes its own
    /// [`NetPolicy`] (settings-backed toggles, Puddle's endpoint registry) with
    /// [`Self::with_address_check`], and its audit (the store) with
    /// [`Self::with_connection_log`]; until then connections are not recorded.
    #[must_use]
    pub fn new(policy: Arc<dyn Policy>, sink: Arc<dyn EventSink>) -> Self {
        Self {
            policy,
            resolver: Arc::new(Coalescing::new(SystemResolver)),
            records: Arc::new(SystemRecords),
            addresses: Arc::new(NetPolicy::new(Arc::new(LocalAccess::NONE))),
            log: Arc::new(NullConnectionLog),
            upstream: None,
            terminations: None,
            git_ssh: None,
            ssh_agents: None,
            guest: None,
            pki: Arc::default(),
            port_probe: None,
            sink,
            reports: None,
            config: ProxyConfig::default(),
            #[cfg(feature = "testing")]
            upstream_ended: Arc::default(),
        }
    }

    /// How many connections to real servers have ended so far (feature `testing`): a test that
    /// cut one waits for this to rise instead of sleeping until the proxy has noticed.
    #[cfg(feature = "testing")]
    #[must_use]
    pub fn upstream_connections_ended(&self) -> u64 {
        self.upstream_ended
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Passes the guest agents' reports (an entrypoint exit, stub DNS trouble) to `reports`, each
    /// with the workspace of the route it came on. Without it they are only logged.
    #[must_use]
    pub fn with_agent_reports(mut self, reports: Arc<dyn AgentReports>) -> Self {
        self.reports = Some(reports);
        self
    }

    /// Lets a plain-HTTP `GET` or `HEAD` of one of `points` through without a rule or a toggle
    /// when its host resolves only to company-network or private addresses: the CRL and
    /// authority-information-access URLs of the roots the host synced.
    #[must_use]
    pub fn with_pki_distribution_points(
        mut self,
        points: puddle_netpolicy::PkiDistributionPoints,
    ) -> Self {
        self.pki = Arc::new(points);
        self
    }

    /// Probes with `probe` whether this machine can reach `ssh.dev.azure.com:22` when a workspace
    /// asks for it, so the refusal can say that the company network blocks the port.
    /// Without a probe the refusal is the ordinary one.
    #[must_use]
    pub fn with_port_probe(mut self, probe: Arc<dyn crate::probe::PortProbe>) -> Self {
        self.port_probe = Some(probe);
        self
    }

    /// Uses `resolver` for allowed names.
    #[must_use]
    pub fn with_resolver(mut self, resolver: Arc<dyn Resolver>) -> Self {
        self.resolver = resolver;
        self
    }

    /// Uses `records` for the `SRV`, `TXT` and `MX` lookups of the guest's stub DNS.
    #[must_use]
    pub fn with_record_resolver(mut self, records: Arc<dyn RecordResolver>) -> Self {
        self.records = records;
        self
    }

    /// Uses `check` for resolved addresses.
    #[must_use]
    pub fn with_address_check(mut self, check: Arc<dyn AddressCheck>) -> Self {
        self.addresses = check;
        self
    }

    /// Reports every connection that got as far as a destination to `log` (the store's
    /// `connection` audit records, R-24): one event when it ends, with its decision, the address
    /// connected to and the bytes each way.
    #[must_use]
    pub fn with_connection_log(mut self, log: Arc<dyn ConnectionLog>) -> Self {
        self.log = log;
        self
    }

    /// Sends admitted connections out along the company proxy route instead of straight to the
    /// destination. Admission is unchanged: the rules, the address guard and the IP rules
    /// all run first, and `DIRECT` hops connect only to an address that passed.
    #[must_use]
    pub fn with_upstream(mut self, upstream: Upstream) -> Self {
        self.upstream = Some(upstream);
        self
    }

    /// Terminates the TLS of the hosts each workspace has a credential for (`source` says which
    /// workspace terminates what) and verifies the real servers with `tls`. Everything else is
    /// spliced as before. See [`crate::terminate`] for what a terminated connection does.
    #[must_use]
    pub fn with_termination(
        mut self,
        source: Arc<dyn TerminationSource>,
        tls: puddle_upstream::TlsClient,
    ) -> Self {
        self.terminations = Some(TerminationConfig { source, tls });
        self
    }

    /// Serves SSH to the Git hosts the workspace terminates (see [`crate::ssh_git::GitSsh`]): Puddle's own
    /// SSH server with `git`'s host keys, each command carried out by its upstream. Without it
    /// every SSH connection is refused (`ssh_unsupported`).
    #[must_use]
    pub fn with_git_ssh(mut self, git: crate::ssh_git::GitSsh) -> Self {
        self.git_ssh = Some(git);
        self
    }

    /// Serves the guest's agent streams through the filtered SSH agent `agents` (see
    /// [`crate::ssh_agent`]). Without it an agent stream is closed, and the agent is off for every
    /// host whatever the terminations say.
    #[must_use]
    pub fn with_ssh_agent(mut self, agents: Arc<crate::ssh_agent::SshAgents>) -> Self {
        self.ssh_agents = Some(agents);
        self
    }

    /// The watch on a spliced connection's key exchange, for SSH announced to a host the
    /// workspace has the filtered agent on for.
    fn kex_watch(
        &self,
        request: &EgressRequest,
        termination: Option<&terminate::Termination>,
    ) -> Option<crate::ssh_agent::KexWatch> {
        let agents = self.ssh_agents.as_ref()?;
        let on = request.protocol == Some(ProtocolHint::Ssh)
            && termination?.ssh_agent_hosts().contains_key(&request.host);
        on.then(|| {
            crate::ssh_agent::KexWatch::new(
                Arc::clone(agents),
                request.workspace.clone(),
                request.host.clone(),
            )
        })
    }

    /// The Git SSH server and host for `request`, when SSH to its name is served by Puddle: a
    /// built-in Git host, or a host of one of the workspace's Git credentials.
    fn git_ssh_for(
        &self,
        request: &EgressRequest,
    ) -> Option<(
        &crate::ssh_git::GitSsh,
        Arc<terminate::Termination>,
        crate::ssh_git::GitHost,
    )> {
        let git = self.git_ssh.as_ref()?;
        let termination = self.termination_of(request)?;
        let host = crate::ssh_git::built_in(&request.host).or_else(|| {
            termination
                .git_ssh_hosts()
                .contains(&request.host)
                .then(|| crate::ssh_git::GitHost {
                    https: request.host.clone(),
                    form: crate::ssh_git::PathForm::Plain,
                })
        })?;
        Some((git, termination, host))
    }

    /// Passes what each workspace's agent says about the guest (the way to open a stream to it,
    /// the ports it listens on) to `watcher`. Without one the proxy ignores both.
    #[must_use]
    pub fn with_guest_watcher(mut self, watcher: Arc<dyn GuestWatcher>) -> Self {
        self.guest = Some(watcher);
        self
    }

    /// Uses `config` for limits and timeouts.
    #[must_use]
    pub fn with_config(mut self, config: ProxyConfig) -> Self {
        self.config = config;
        self
    }

    /// The limits in use.
    #[must_use]
    pub fn config(&self) -> &ProxyConfig {
        &self.config
    }

    /// The [`StreamHandler`] for one workspace's route. `workspace` comes from the route, never from
    /// the guest. Each handler has its own stream cap, so give each route one handler.
    #[must_use]
    pub fn handler(self: &Arc<Self>, workspace: WorkspaceId) -> WorkspaceHandler {
        WorkspaceHandler {
            proxy: Arc::clone(self),
            streams: Arc::new(Semaphore::new(self.config.max_streams_per_workspace)),
            h2_streams: Arc::new(Semaphore::new(self.config.max_h2_streams_per_workspace)),
            lookups: Arc::new(Semaphore::new(self.config.max_lookups_per_workspace)),
            workspace,
        }
    }

    pub(crate) fn upstream(&self) -> Option<&Upstream> {
        self.upstream.as_ref()
    }

    /// The termination of `request`'s workspace, if this connection is to be terminated: a
    /// `CONNECT` to a name of the workspace's termination set on port 443.
    pub(crate) fn terminating(
        &self,
        request: &EgressRequest,
        from_container: bool,
    ) -> Option<(
        Arc<terminate::Termination>,
        terminate::Mode,
        &puddle_upstream::TlsClient,
    )> {
        let terminations = self.terminations.as_ref()?;
        if request.port != terminate::request::HTTPS_PORT {
            return None;
        }
        let termination = terminations.source.termination(&request.workspace)?;
        let mode = termination.mode_for(&request.host, from_container)?;
        Some((termination, mode, &terminations.tls))
    }

    /// The workspace's termination, which also holds its rules on method and path.
    fn termination_of(&self, request: &EgressRequest) -> Option<Arc<terminate::Termination>> {
        self.terminations
            .as_ref()?
            .source
            .termination(&request.workspace)
    }

    /// Why a host that the workspace would decrypt (it decrypts every host) is passed through
    /// instead: on its never-decrypt list, or one its tools did not accept the certificate for.
    fn skipped(&self, request: &EgressRequest, from_container: bool) -> Option<&'static str> {
        let terminations = self.terminations.as_ref()?;
        if request.port != terminate::request::HTTPS_PORT {
            return None;
        }
        let termination = terminations.source.termination(&request.workspace)?;
        termination.skipped(&request.host, from_container)
    }

    pub(crate) fn addresses(&self) -> &dyn AddressCheck {
        self.addresses.as_ref()
    }

    pub(crate) fn resolver(&self) -> &dyn Resolver {
        self.resolver.as_ref()
    }

    pub(crate) fn record_resolver(&self) -> &dyn RecordResolver {
        self.records.as_ref()
    }

    /// Whether a name this host can't resolve may be left to the company proxy.
    pub(crate) fn company_proxy_may_resolve(&self) -> bool {
        self.upstream
            .as_ref()
            .is_some_and(Upstream::resolves_unknown_names)
    }

    /// Whether `request`'s name is one whose SSH Puddle serves itself or refuses, never splices:
    /// a built-in Git host or a host of one of the workspace's Git credentials.
    pub(crate) fn is_git_ssh_name(&self, request: &EgressRequest) -> bool {
        crate::ssh_git::built_in(&request.host).is_some()
            || self
                .termination_of(request)
                .is_some_and(|t| t.git_ssh_hosts().contains(&request.host))
    }

    /// Whether `request` is SSH to a Git host's name that Puddle does not serve: refused at once,
    /// before the rules (SSH to any other name is spliced after a rules decision).
    pub(crate) fn refuses_ssh(&self, request: &EgressRequest) -> bool {
        request.protocol == Some(ProtocolHint::Ssh) && self.is_git_ssh_name(request)
    }

    /// The refusal text for SSH to Azure DevOps when this machine's own probe of the port failed
    ///; `None` for any other destination, port or answer, and without a probe.
    async fn azure_devops_ssh_message(&self, request: &EgressRequest) -> Option<String> {
        let probe = self.port_probe.as_ref()?;
        let is_azure = matches!(&request.host, Host::Name(name) if name.as_str() == puddle_netpolicy::AZURE_DEVOPS_SSH_HOST);
        if !is_azure || request.port != 22 {
            return None;
        }
        let reachable = probe
            .reachable(puddle_netpolicy::AZURE_DEVOPS_SSH_HOST, 22)
            .await;
        (!reachable).then(puddle_netpolicy::azure_devops_ssh_message)
    }

    /// A plain-HTTP `GET` or `HEAD` of a certificate distribution point named in a synced root
    ///: `Some` admits it with its record (`pki_distribution_point`, no rule, no toggle)
    /// when the host is a name or literal that resolves, and every address is a company-network or
    /// private one. `None` leaves the request to the ordinary decision, which is also what a
    /// public, late or missing answer gets.
    pub(crate) async fn admit_distribution_point(
        &self,
        request: &EgressRequest,
        method: &str,
        path: &str,
    ) -> Option<(Admitted, ConnectionEvent)> {
        if !self.pki.covers(&request.host, request.port, method, path) {
            return None;
        }
        let addrs = resolve_destination(self, &request.host, request.port)
            .await
            .ok()?;
        if !addrs
            .iter()
            .all(|addr| self.addresses.pki_address(&request.host, *addr))
        {
            return None;
        }
        let mut event = ConnectionEvent::new(
            request,
            ConnectionDecision::Allow,
            ConnectionReason::PkiDistributionPoint,
        );
        event.company_network = addrs
            .iter()
            .find_map(|addr| self.addresses.company_signal(&request.host, *addr));
        tracing::info!(workspace = %request.workspace, host = %request.host, port = request.port, "certificate distribution point allowed");
        Some((Admitted::checked(addrs, true), event))
    }

    /// The rule that decides `request`, without recording anything (on a blocking thread: the
    /// policy may read SQLite).
    pub(crate) async fn look_up_rule(
        &self,
        request: EgressRequest,
        suffix_allows: SuffixAllows,
    ) -> Result<Option<Decision>, PolicyError> {
        let policy = Arc::clone(&self.policy);
        tokio::task::spawn_blocking(move || policy.lookup(&request, suffix_allows))
            .await
            .unwrap_or_else(|_| {
                Err(PolicyError {
                    reason: "policy task failed".into(),
                })
            })
    }

    pub(crate) async fn decide(
        &self,
        request: &EgressRequest,
        suffix_allows: SuffixAllows,
    ) -> Result<Decision, PolicyError> {
        if self.refuses_ssh(request) {
            return Ok(Decision::Blocked {
                reason: BlockReason::SshUnsupported,
            });
        }
        let policy = Arc::clone(&self.policy);
        let request = request.clone();
        // A miss writes a pending row, so the call may block on SQLite.
        tokio::task::spawn_blocking(move || policy.decide(&request, suffix_allows))
            .await
            .unwrap_or_else(|_| {
                Err(PolicyError {
                    reason: "policy task failed".into(),
                })
            })
    }

    /// Hands `event` to the connection log on a blocking thread (it may write to SQLite).
    pub(crate) async fn record(&self, event: ConnectionEvent) {
        let log = Arc::clone(&self.log);
        if tokio::task::spawn_blocking(move || log.record(&event))
            .await
            .is_err()
        {
            tracing::warn!("connection log panicked; record lost");
        }
    }
}

/// What terminated connections need, set with [`Proxy::with_termination`].
struct TerminationConfig {
    source: Arc<dyn TerminationSource>,
    tls: puddle_upstream::TlsClient,
}

impl std::fmt::Debug for TerminationConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TerminationConfig").finish_non_exhaustive()
    }
}

/// Serves the proxied connections of one workspace. Made by [`Proxy::handler`].
#[derive(Debug, Clone)]
pub struct WorkspaceHandler {
    pub(crate) proxy: Arc<Proxy>,
    pub(crate) streams: Arc<Semaphore>,
    pub(crate) h2_streams: Arc<Semaphore>,
    pub(crate) lookups: Arc<Semaphore>,
    pub(crate) workspace: WorkspaceId,
}

impl WorkspaceHandler {
    /// The workspace this handler serves.
    #[must_use]
    pub fn workspace(&self) -> &WorkspaceId {
        &self.workspace
    }
}

impl StreamHandler for WorkspaceHandler {
    async fn handle(&self, stream: GuestStream) {
        let span = tracing::debug_span!("conn", workspace = %self.workspace);
        serve(self, stream).instrument(span).await;
    }

    async fn resolve(&self, query: ResolveQuery) -> ResolveAnswer {
        let span = tracing::debug_span!("resolve", workspace = %self.workspace);
        self.resolve_name(query).instrument(span).await
    }

    async fn udp_refused(&self, refusal: UdpRefusal) {
        self.record_udp_refusal(&refusal).await;
    }

    fn agent_report(&self, report: AgentReport) -> impl Future<Output = ()> + Send {
        if let Some(reports) = &self.proxy.reports {
            reports.report(&self.workspace, report);
        }
        std::future::ready(())
    }

    fn guest_link(&self, link: GuestLink) -> impl Future<Output = ()> + Send {
        if let Some(watcher) = &self.proxy.guest {
            watcher.link(&self.workspace, link);
        }
        std::future::ready(())
    }

    fn guest_ports(&self, ports: Vec<u16>) -> impl Future<Output = ()> + Send {
        if let Some(watcher) = &self.proxy.guest {
            watcher.ports(&self.workspace, ports);
        }
        std::future::ready(())
    }

    async fn ssh_agent(&self, stream: AgentStream) {
        let Some(agents) = &self.proxy.ssh_agents else {
            tracing::warn!(workspace = %self.workspace, "ssh-agent stream with no filtered agent on this host, closed");
            return;
        };
        let Ok(_permit) = Arc::clone(&self.streams).try_acquire_owned() else {
            tracing::warn!(workspace = %self.workspace, "ssh-agent stream over the stream limit, closed");
            return;
        };
        let terminations = self
            .proxy
            .terminations
            .as_ref()
            .map(|t| Arc::clone(&t.source));
        crate::ssh_agent::serve(
            Arc::clone(agents),
            terminations,
            self.workspace.clone(),
            stream,
            crate::ssh_agent::IDLE_LIMIT,
        )
        .await;
    }
}

impl Proxy {
    /// Tells the guest watcher that an agent session of `workspace` ended.
    pub(crate) fn session_ended(&self, workspace: &WorkspaceId) {
        if let Some(watcher) = &self.guest {
            watcher.session_ended(workspace);
        }
    }
}

impl WorkspaceHandler {
    /// Audits UDP the guest's capture rules refused: a `blocked` record with reason
    /// `udp_unsupported`, never a pending row (nothing can approve it). The report is the guest's
    /// claim, so only a plain IP address is taken as a destination, and the audit's own rate limit
    /// (R-26) bounds the records.
    async fn record_udp_refusal(&self, refusal: &UdpRefusal) {
        let Ok(ip) = refusal.address.parse::<IpAddr>() else {
            tracing::debug!(workspace = %self.workspace, "UDP refusal report without an IP address ignored");
            return;
        };
        let request = EgressRequest::new(self.workspace.clone(), Host::Ip(ip), refusal.port);
        let event = ConnectionEvent::new(
            &request,
            ConnectionDecision::Blocked,
            ConnectionReason::Blocked(BlockReason::UdpUnsupported),
        );
        self.proxy.record(event).await;
    }
}

/// An error response: status, extra headers, a one-line explanation for the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Refusal {
    pub(crate) status: std::borrow::Cow<'static, str>,
    pub(crate) headers: Vec<(&'static str, String)>,
    pub(crate) message: String,
    /// The reason the connection record carries, for a refusal Puddle made on purpose (not an
    /// upstream failure): also sent as `x-puddle-blocked`.
    pub(crate) code: Option<&'static str>,
    /// How reaching the destination went, for a refusal that is a failure to get there (not a
    /// decision): the connection record's `outcome`.
    pub(crate) outcome: Option<ConnectionOutcome>,
}

impl Refusal {
    pub(crate) fn new(
        status: impl Into<std::borrow::Cow<'static, str>>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            status: status.into(),
            headers: Vec::new(),
            message: message.into(),
            code: None,
            outcome: None,
        }
    }

    /// Marks this refusal as a failure to reach the destination, which the connection record
    /// carries as `outcome`.
    pub(crate) fn failed(mut self, outcome: ConnectionOutcome) -> Self {
        self.outcome = Some(outcome);
        self
    }

    /// Marks this refusal as one Puddle made on purpose: the connection record gets `code` as its
    /// reason, and the guest reads it in `x-puddle-blocked`.
    pub(crate) fn coded(mut self, code: &'static str) -> Self {
        self.code = Some(code);
        self.header("x-puddle-blocked", code)
    }

    pub(crate) fn header(mut self, name: &'static str, value: impl Into<String>) -> Self {
        self.headers.push((name, value.into()));
        self
    }

    fn bytes(&self) -> Vec<u8> {
        let body = format!("puddle: {}\n", self.message);
        let mut out = format!(
            "HTTP/1.1 {}\r\ncontent-type: text/plain; charset=utf-8\r\ncontent-length: {}\r\n",
            self.status,
            body.len()
        );
        for (name, value) in &self.headers {
            out.push_str(name);
            out.push_str(": ");
            out.push_str(value);
            out.push_str("\r\n");
        }
        out.push_str("connection: close\r\n\r\n");
        out.push_str(&body);
        out.into_bytes()
    }
}

/// Sends `refusal` and closes the stream cleanly: a refusal is a normal end, and dropping the
/// stream without the shutdown would reset it, which can reach the client before the response.
pub(crate) async fn refuse<S: AsyncWrite + Unpin>(stream: &mut S, refusal: &Refusal) {
    let sent = async {
        stream.write_all(&refusal.bytes()).await?;
        stream.shutdown().await
    };
    if let Err(err) = sent.await {
        tracing::debug!(error = %err, "guest went away before the refusal was sent");
    }
}

async fn serve(handler: &WorkspaceHandler, stream: GuestStream) {
    let proxy = &handler.proxy;
    let (stream, counts) = Counted::new(stream);
    let mut reader = BufReader::new(stream);
    let Ok(_permit) = Arc::clone(&handler.streams).try_acquire_owned() else {
        refuse_over_limit(handler, reader).await;
        return;
    };
    let head = match read_request(&mut reader, proxy.config.head_timeout).await {
        Ok(head) => head,
        Err(None) => return,
        Err(Some(refusal)) => {
            refuse(reader.get_mut(), &refusal).await;
            return;
        }
    };
    let (target, path, _) = match parse_request(&head) {
        Ok(parsed) => parsed,
        Err(refusal) => {
            refuse(reader.get_mut(), &refusal).await;
            return;
        }
    };
    if let Some(path) = path {
        match plain::Incoming::new(head, target, path) {
            Ok(first) => plain::serve(handler, reader, &counts, first).await,
            Err(refusal) => refuse(reader.get_mut(), &refusal).await,
        }
        return;
    }
    let mut request =
        EgressRequest::new(handler.workspace.clone(), target.host.clone(), target.port);
    if ssh::announced(&head) || ssh::started(reader.buffer()) {
        request = request.with_protocol(ProtocolHint::Ssh);
    }
    let mut event = relay(handler, reader, &request, &head, &target).await;
    event.bytes_up = counts.read();
    event.bytes_down = counts.written();
    proxy.record(event).await;
}

/// How long a connection over the workspace's limit may take to say where it was going. Short:
/// it holds a task and a descriptor but no slot.
const OVER_LIMIT_HEAD_TIMEOUT: Duration = Duration::from_secs(5);

/// Refuses a connection over the workspace's limit with a `503` that says why. The head is read
/// first (briefly) so the refusal reaches the audit with its destination: the user then sees
/// which connections were turned away, not just a warning in the log.
async fn refuse_over_limit(handler: &WorkspaceHandler, mut reader: GuestReader) {
    let proxy = &handler.proxy;
    let limit = proxy.config.max_streams_per_workspace;
    tracing::warn!(
        limit,
        workspace = %handler.workspace,
        "connection refused: workspace over its connection limit"
    );
    let wait = proxy.config.head_timeout.min(OVER_LIMIT_HEAD_TIMEOUT);
    let target = match read_request(&mut reader, wait).await {
        Ok(head) => parse_request(&head).ok().map(|(target, ..)| target),
        Err(_) => None,
    };
    let refusal = Refusal::new(
        "503 Service Unavailable",
        format!(
            "too many open connections from this workspace (limit {limit}); \
             retry when some have closed"
        ),
    );
    refuse(reader.get_mut(), &refusal).await;
    // The guest has its answer: end its stream before the audit row, which waits on a blocking
    // thread and can lag, so a flood of refusals does not keep a stream and its buffer each.
    drop(reader);
    if let Some(Target { host, port }) = target {
        let request = EgressRequest::new(handler.workspace.clone(), host, port);
        let event = ConnectionEvent::new(
            &request,
            ConnectionDecision::Blocked,
            ConnectionReason::ConnectionLimit,
        );
        proxy.record(event).await;
    }
}

/// The decision and reason a decrypted connection's end puts on its audit record.
pub(crate) fn apply_reason(event: &mut ConnectionEvent, done: &terminate::Outcome) {
    if let Some(refused) = &done.refused_by_rule {
        // A rule on method and path refused a request: the first one is the one recorded.
        event.decision = ConnectionDecision::Deny;
        event.reason = ConnectionReason::RequestRule;
        event.request_rule = Some(refused.rule);
    } else if done.sni_mismatch {
        event.decision = ConnectionDecision::Blocked;
        event.reason = ConnectionReason::SniMismatch;
    } else if done.certificate_unavailable {
        // Puddle's own failure, not the client's: the rule's decision stands.
        event.reason = ConnectionReason::CertificateUnavailable;
    } else if done.certificate_refused {
        // Puddle blocked nothing: the rule's decision stands and the tool refused.
        event.reason = ConnectionReason::GuestTlsRejected;
    } else if let Some(code) = done.ended {
        // Puddle blocked nothing at the start: the rule's decision stands, a limit cut it short.
        event.reason = ConnectionReason::LimitEnded(code);
    } else if let Some(code) = done.refused {
        event.decision = ConnectionDecision::Blocked;
        event.reason = ConnectionReason::Refused(code);
    } else if let Some(code) = done.passed_through {
        event.reason = ConnectionReason::PassedThrough(code);
    }
}

/// What the record of a connection says once a rule on method and path has decided about it.
type RecordChange = Box<dyn FnOnce(&mut ConnectionEvent) + Send>;

/// Applies the workspace's rules on method and path to what the proxy can read without
/// decrypting: a plain-HTTP request, and (because a tunnel's content is not readable) a tunnel
/// to a covered host on a port that is not decrypted. `None`: nothing refuses it.
pub(crate) fn refuse_by_request_rule(
    proxy: &Proxy,
    request: &EgressRequest,
    head: &Head,
    path: Option<&str>,
) -> Option<(Refusal, RecordChange)> {
    let termination = proxy.termination_of(request)?;
    let rules = termination.request_rules();
    let covering = rules.covering(&request.host)?;
    let Some(path) = path else {
        if request.port == terminate::request::HTTPS_PORT {
            return None;
        }
        // Only port 443 is decrypted, so a tunnel to another port of a covered host would carry
        // requests the rules never see.
        let code = "request_rule_port";
        let refusal = Refusal::new(
            "403 Forbidden",
            format!(
                "{} has rules on method and path (request rule {covering}), which Puddle can only apply to HTTPS on port 443 and to plain HTTP: a tunnel to port {} is refused. Use port 443, or delete the host's rules on method and path to open other ports",
                request.host, request.port
            ),
        )
        .coded(code)
        .header("x-puddle-request-rule", covering.to_string());
        let change: RecordChange = Box::new(move |event| {
            event.decision = ConnectionDecision::Blocked;
            event.reason = ConnectionReason::Refused(code);
            event.request_rule = Some(covering);
        });
        return Some((refusal, change));
    };
    let view = terminate::RequestView::new(&head.method, path, &head.headers);
    let rule = rules.refusing(&request.host, &view)?;
    let line = HttpRequestLine::new(head.method.as_str(), path);
    let refusal = terminate::request_rules::refusal(rule, &head.method, &request.host);
    let change: RecordChange = Box::new(move |event| {
        event.decision = ConnectionDecision::Deny;
        event.reason = ConnectionReason::RequestRule;
        event.request_rule = Some(rule);
        event.http = Some(line);
    });
    Some((refusal, change))
}

/// The port SSH listens on.
const GIT_SSH_PORT: u16 = 22;

/// Decides, connects and relays one `CONNECT` whose destination is known; returns what the audit
/// records about it (bytes are filled in by the caller once the stream is gone). Plain HTTP goes
/// to [`plain::serve`] instead.
async fn relay(
    handler: &WorkspaceHandler,
    mut reader: GuestReader,
    request: &EgressRequest,
    head: &Head,
    target: &Target,
) -> ConnectionEvent {
    let proxy = &handler.proxy;
    let from_container = ssh::from_container(head);
    // A client that waits for the server's identification before it sends its own (`ssh-keyscan`)
    // sends nothing that marks it as SSH. Port 22 of a Git host whose SSH Puddle serves is SSH, so
    // such a client gets Puddle's server and its keys, as `ssh` does.
    if (request.protocol == Some(ProtocolHint::Ssh) || request.port == GIT_SSH_PORT)
        && let Some((git, termination, host)) = proxy.git_ssh_for(request)
    {
        return crate::ssh_git::serve(handler, reader, request, git, &termination, host).await;
    }
    let (admitted, mut event) = admit(proxy, request).await;
    let admitted = match admitted {
        Ok(admitted) => admitted,
        Err(refusal) => {
            event.outcome = refusal.outcome;
            refuse(reader.get_mut(), &refusal).await;
            return event;
        }
    };
    if let Some(refused) = refuse_by_request_rule(proxy, request, head, None) {
        let (refusal, change) = refused;
        change(&mut event);
        refuse(reader.get_mut(), &refusal).await;
        return event;
    }
    let mut guest = Opened::from(reader);
    let terminating = proxy.terminating(request, from_container);
    // SSH to a name that is not a Git host is spliced, even on port 443 of a decrypted name. The
    // header that says SSH is the guest's word, so there the first bytes decide: an identification
    // line is spliced, anything else is decrypted like every connection to the name.
    if terminating.is_some() && request.protocol == Some(ProtocolHint::Ssh) {
        guest.read_first(proxy.config.head_timeout).await;
    }
    // A bound host is terminated: the guest's TLS handshake comes first, so a client that asks
    // for the wrong name never causes a connection to the real server.
    if let Some((termination, mode, tls)) = terminating.filter(|_| !ssh::started(&guest.early)) {
        let cx = Arc::new(terminate::Context {
            proxy: Arc::clone(proxy),
            termination,
            mode,
            tls: tls.clone(),
            workspace: request.workspace.clone(),
            target: target.clone(),
            admitted,
            connections: Arc::clone(&handler.streams),
            streams: Arc::clone(&handler.h2_streams),
        });
        let done = terminate::run(cx, guest).await;
        apply_reason(&mut event, &done);
        event.http = done.http;
        if let Some(refused) = &done.refused_by_rule {
            event.http = Some(refused.line.clone());
        }
        event.resolved_ip = done.resolved_ip;
        event.upstream = done.hop;
        event.outcome = done.connect;
        event.injected = done.injected;
        event.binding_id = done.binding_id;
        event.unswapped_stand_ins = done.unswapped_stand_ins;
        event.scrubbed_stand_ins = done.scrubbed_stand_ins;
        event.foreign_credentials = done.foreign_credentials;
        event.answer_credentials = done.answer_credentials;
        if done.ssh {
            note_block(&mut event, BlockReason::SshUnsupported);
        }
        return event;
    }
    let out = match connect_out(
        proxy.upstream.as_ref(),
        target,
        true,
        &admitted,
        proxy.config.connect_timeout,
    )
    .await
    {
        Ok(out) => out,
        Err(refusal) => {
            event.outcome = refusal.outcome;
            // A guest that has its `200` already gets no second answer: the connection ends.
            if !guest.answered {
                refuse(&mut guest.stream, &refusal).await;
            }
            return event;
        }
    };
    event.outcome = Some(ConnectionOutcome::Connected);
    event.resolved_ip = out.addr.map(|addr| addr.ip());
    event.upstream = out.hop;
    if let Some(code) = proxy.skipped(request, from_container) {
        event.reason = ConnectionReason::PassedThrough(code);
    }
    note_tunnel(proxy, guest, (out.stream, out.watch), request, &mut event).await;
    event
}

/// Splices a `CONNECT` tunnel that is not decrypted, and puts on `event` what it saw.
async fn note_tunnel(
    proxy: &Proxy,
    guest: Opened<Counted<GuestStream>>,
    (stream, watch): (TcpStream, Option<RouteWatch>),
    request: &EgressRequest,
    event: &mut ConnectionEvent,
) {
    let termination = proxy.termination_of(request);
    let kex = proxy.kex_watch(request, termination.as_deref());
    let end = tunnel(
        guest,
        stream,
        Some(request).filter(|request| proxy.is_git_ssh_name(request)),
        watch,
        termination,
        kex,
    )
    .await;
    event.http = end.http;
    if end.ssh {
        note_block(event, BlockReason::SshUnsupported);
    }
    if end.token_endpoint {
        event.decision = ConnectionDecision::Blocked;
        event.reason = ConnectionReason::Refused(token_gate::CODE);
    }
}

/// The client side of one proxied stream, counted for the audit.
pub(crate) type ClientReader<S> = BufReader<Counted<S>>;

/// The guest side of one proxied stream.
type GuestReader = ClientReader<GuestStream>;

/// Reads the head within `limit`. `Err(None)`: the client closed or failed before a request, so
/// there is no one to answer.
pub(crate) async fn read_request<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    limit: Duration,
) -> Result<Head, Option<Refusal>> {
    match tokio::time::timeout(limit, http::read_head(reader)).await {
        Err(_) => Err(Some(Refusal::new(
            "408 Request Timeout",
            format!("request head not received within {}s", limit.as_secs()),
        ))),
        Ok(Err(err)) => {
            tracing::debug!(error = %err, "guest stream failed before a request");
            Err(None)
        }
        Ok(Ok(Err(HeadError::Empty))) => Err(None),
        Ok(Ok(Err(HeadError::TooLarge))) => Err(Some(Refusal::new(
            "431 Request Header Fields Too Large",
            format!("request head over {} KiB", http::MAX_HEAD / 1024),
        ))),
        Ok(Ok(Err(HeadError::Bad(why)))) => Err(Some(Refusal::new("400 Bad Request", why))),
        Ok(Ok(Ok(head))) => Ok(head),
    }
}

/// The normalised target, the origin-form path for a plain-HTTP request, and its body framing.
pub(crate) fn parse_request(head: &Head) -> Result<(Target, Option<String>, Body), Refusal> {
    let RawTarget { host, port, path } =
        http::parse_target(&head.method, &head.uri).map_err(|why| {
            Refusal::new(
                "400 Bad Request",
                format!(
                    "{why}; point the client at this proxy (CONNECT or an absolute http:// URI)"
                ),
            )
        })?;
    let host = normalise_host(&host)
        .map_err(|why| Refusal::new("400 Bad Request", why.to_string()))?
        .into_host();
    let body = if head.is_connect() {
        Body::None
    } else {
        http::body_framing(&head.headers).map_err(|why| Refusal::new("400 Bad Request", why))?
    };
    Ok((Target { host, port }, path, body))
}

/// Decides the request and, if allowed, returns the addresses it may connect to (R-10, R-14),
/// with the audit event for the decision.
pub(crate) async fn admit(
    proxy: &Proxy,
    request: &EgressRequest,
) -> (Result<Admitted, Refusal>, ConnectionEvent) {
    // Replaced by the first step that decides; a path that forgets to stays fail-closed in the
    // audit too.
    let mut event = ConnectionEvent::new(
        request,
        ConnectionDecision::Blocked,
        ConnectionReason::PolicyUnavailable,
    );
    let admitted = admit_into(proxy, request, &mut event).await;
    (admitted, event)
}

/// Records each decision `admit` takes in `event`.
fn note(
    event: &mut ConnectionEvent,
    request: &EgressRequest,
    decided: &Result<Decision, PolicyError>,
) {
    *event = match decided {
        Ok(decision) => ConnectionEvent::decided(request, decision),
        Err(_) => ConnectionEvent::new(
            request,
            ConnectionDecision::Blocked,
            ConnectionReason::PolicyUnavailable,
        ),
    };
}

/// Marks `event` blocked for `reason`, keeping the rule that allowed the name, if any.
fn note_block(event: &mut ConnectionEvent, reason: BlockReason) {
    event.decision = ConnectionDecision::Blocked;
    event.reason = ConnectionReason::Blocked(reason);
    event.pending_id = None;
}

/// The addresses of the destination: an IP literal is its own, a name is resolved with the
/// proxy's timeout. Nothing resolved (or no answer in time) is the early end [`unresolved`]
/// gives, returned as the error: the destination is left to the company proxy or refused.
async fn resolve_destination(
    proxy: &Proxy,
    host: &Host,
    port: u16,
) -> Result<Vec<SocketAddr>, Result<Admitted, Refusal>> {
    let addrs = match host {
        Host::Ip(ip) => vec![SocketAddr::new(*ip, port)],
        Host::Name(name) => {
            match tokio::time::timeout(
                proxy.config.resolve_timeout,
                proxy.resolver.resolve(name, port),
            )
            .await
            {
                Err(_) => {
                    return Err(unresolved(proxy, host, Unresolved::TimedOut));
                }
                Ok(Err(err)) => {
                    tracing::info!(%host, error = %err, "resolve failed");
                    return Err(unresolved(proxy, host, Unresolved::from_error(&err)));
                }
                Ok(Ok(addrs)) => addrs,
            }
        }
    };
    if addrs.is_empty() {
        return Err(unresolved(proxy, host, Unresolved::NoAddresses));
    }
    Ok(addrs)
}

async fn admit_into(
    proxy: &Proxy,
    request: &EgressRequest,
    event: &mut ConnectionEvent,
) -> Result<Admitted, Refusal> {
    let host = &request.host;
    let port = request.port;
    // SSH to a Git host's name is refused whatever the destination is: before the name stage (so
    // the reason is the protocol, not a toggle that turning on would only lead to the same
    // refusal) and before any rule, so it never becomes a pending row or an allow. SSH to any
    // other name is decided below like every connection.
    if proxy.refuses_ssh(request) {
        note_block(event, BlockReason::SshUnsupported);
        let mut refusal = block(request, &[BlockReason::SshUnsupported]);
        if let Some(message) = proxy.azure_devops_ssh_message(request).await {
            refusal.message = message;
        }
        return Err(refusal);
    }
    // Name stage: a literal or a name that is blocked by itself never reaches the rules, so it
    // never becomes a pending row (R-14).
    let target = puddle_netpolicy::Target::from_host(host.clone());
    if let Some(reason) = proxy
        .addresses
        .check_target(&request.workspace, &target, port)
    {
        note_block(event, reason);
        return Err(block(request, &[reason]));
    }
    let decided = proxy.decide(request, SuffixAllows::Count).await;
    note(event, request, &decided);
    let pattern = allowed(request, decided)?;
    let addrs = match resolve_destination(proxy, host, port).await {
        Ok(addrs) => addrs,
        Err(done) => return done,
    };
    let resolved = addrs.len();
    let (addrs, ip_allows) = apply_ip_rules(proxy, request, addrs, event)?;
    let mut usable = Vec::new();
    let mut exact_only = Vec::new();
    let mut categories: Vec<LocalCategory> = Vec::new();
    let mut blocked = Vec::new();
    let mut refused_first = None;
    let mut company_ip = None;
    for addr in addrs {
        if event.company_network.is_none() {
            event.company_network = proxy.addresses.company_signal(host, addr);
        }
        match proxy.addresses.check_named(&request.workspace, host, addr) {
            AddressVerdict::Allow => usable.push(addr),
            AddressVerdict::ExactOnly(category) => {
                exact_only.push(addr);
                categories.push(category);
            }
            AddressVerdict::Block(reason) => {
                if matches!(reason, BlockReason::CompanyNetwork(_)) {
                    company_ip.get_or_insert(addr.ip());
                }
                refused_first.get_or_insert((addr.ip(), RefusedReason::Guard(reason)));
                blocked.push(reason);
            }
        }
    }
    event.refused_address = event.refused_address.or(refused_first);
    if pattern == PatternKind::Exact {
        // An exact allow reaches local addresses whose toggle is on.
        usable.append(&mut exact_only);
    } else if !exact_only.is_empty() {
        // After a wildcard allow, a local address whose toggle is on needs an exact allow of its
        // own (R-14): an exact rule for the address itself admits it.
        let (by_ip, rest): (Vec<_>, Vec<_>) = exact_only.into_iter().partition(|addr| {
            let rule = ip_allows.iter().find(|(allowed, _)| allowed == addr);
            if let Some((_, rule_id)) = rule {
                tracing::info!(workspace = %request.workspace, host = %request.host, %addr, rule = %rule_id, "local address allowed by its IP rule");
            }
            rule.is_some()
        });
        usable.extend(by_ip);
        exact_only = rest;
    }
    if usable.is_empty() && !exact_only.is_empty() {
        // Otherwise only an exact allow of the name counts. Asking again without suffix allows
        // writes the pending row for the exact name.
        let again = proxy.decide(request, SuffixAllows::Ignore).await;
        note(event, request, &again);
        if allowed(request, again).map_err(|r| wildcard_note(r, &categories))? != PatternKind::Exact
        {
            note_block(event, BlockReason::LocalAddress);
            return Err(block(request, &[BlockReason::LocalAddress]));
        }
        usable = exact_only;
    }
    if usable.is_empty() {
        if blocked.is_empty() {
            blocked.push(BlockReason::LocalAddress);
        }
        note_block(event, shown_reason(&blocked));
        return Err(block_at(request, &blocked, company_ip));
    }
    // A proxy may be told the name only when nothing the name resolved to was dropped: otherwise
    // it could pick the dropped address itself (R-14, R-27).
    let name_ok = matches!(host, Host::Ip(_)) || usable.len() == resolved;
    Ok(Admitted::checked(usable, name_ok))
}

/// How resolving a name here came to nothing.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Unresolved {
    TimedOut,
    /// The resolver answered that the name does not exist ([`io::ErrorKind::NotFound`]).
    NoSuchName,
    /// The lookup itself failed (no resolver answered, the network is down): the name may exist.
    Failed,
    NoAddresses,
}

impl Unresolved {
    /// What an error from [`Resolver::resolve`] means, by the `NotFound` contract.
    pub(crate) fn from_error(err: &io::Error) -> Self {
        if err.kind() == io::ErrorKind::NotFound {
            Self::NoSuchName
        } else {
            Self::Failed
        }
    }

    /// The reason for the log and the response the guest gets: a `504` for a timeout and a `502`
    /// for anything else, each saying which.
    pub(crate) fn refusal(self, host: &Host) -> (&'static str, Refusal) {
        let (why, status, message, outcome) = match self {
            Self::TimedOut => (
                "timed out",
                "504 Gateway Timeout",
                format!("resolving {host} timed out"),
                ConnectionOutcome::Timeout,
            ),
            Self::NoSuchName => (
                "no such name",
                "502 Bad Gateway",
                format!("no such name: {host} does not exist"),
                ConnectionOutcome::ConnectFailed,
            ),
            Self::Failed => (
                "failed",
                "502 Bad Gateway",
                format!("the lookup for {host} failed, so it is not known whether the name exists"),
                ConnectionOutcome::ConnectFailed,
            ),
            Self::NoAddresses => (
                "returned no addresses",
                "502 Bad Gateway",
                format!("{host} has no addresses"),
                ConnectionOutcome::ConnectFailed,
            ),
        };
        (why, Refusal::new(status, message).failed(outcome))
    }
}

/// A name this host could not resolve: the company proxy resolves it when that is on, else the
/// guest gets the refusal and the audit says which.
fn unresolved(proxy: &Proxy, host: &Host, how: Unresolved) -> Result<Admitted, Refusal> {
    let (why, refusal) = how.refusal(host);
    left_to_upstream(proxy, host, why).ok_or(refusal)
}

fn left_to_upstream(proxy: &Proxy, host: &Host, why: &str) -> Option<Admitted> {
    if proxy
        .upstream
        .as_ref()
        .is_some_and(Upstream::resolves_unknown_names)
    {
        tracing::info!(%host, why, "name not resolved here; the company proxy resolves it");
        Some(Admitted::unresolved())
    } else {
        None
    }
}

/// Applies each resolved address's own rules (R-27): drops every address an IP rule denies, and
/// returns the rest with the addresses an exact IP rule allows (for R-14). A literal was decided
/// as itself, so only a name's addresses are looked up.
fn apply_ip_rules(
    proxy: &Proxy,
    request: &EgressRequest,
    addrs: Vec<SocketAddr>,
    event: &mut ConnectionEvent,
) -> Result<(Vec<SocketAddr>, Vec<IpRuleHit>), Refusal> {
    let mut ip_allows = Vec::new();
    if matches!(request.host, Host::Ip(_)) {
        return Ok((addrs, ip_allows));
    }
    let mut ip_denies = Vec::new();
    let mut kept = Vec::new();
    for addr in addrs {
        match ip_rule(proxy, request, addr) {
            Ok(IpRule::Allow(rule_id)) => {
                ip_allows.push((addr, rule_id));
                kept.push(addr);
            }
            Ok(IpRule::Deny(rule_id)) => ip_denies.push((addr, rule_id)),
            Ok(IpRule::NoRule) => kept.push(addr),
            Err(err) => {
                tracing::warn!(%addr, error = %err, "IP rule lookup failed, refused");
                note(event, request, &Err(err));
                return Err(unavailable());
            }
        }
    }
    event.refused_address = ip_denies
        .first()
        .map(|(addr, _)| (addr.ip(), RefusedReason::IpRule));
    if kept.is_empty() {
        return Err(ip_denied(request, &ip_denies, event));
    }
    Ok((kept, ip_allows))
}

/// A resolved address and the IP rule that matched it.
type IpRuleHit = (SocketAddr, RuleId);

/// What the rules say about one resolved address by itself.
enum IpRule {
    /// No rule for the address.
    NoRule,
    /// An exact allow: admits a local address after a wildcard allow (R-14).
    Allow(RuleId),
    /// A deny: the address is never used (R-27).
    Deny(RuleId),
}

/// The rule that decides `addr`'s IP for `request`'s workspace. Looks only; a miss records nothing.
fn ip_rule(
    proxy: &Proxy,
    request: &EgressRequest,
    addr: SocketAddr,
) -> Result<IpRule, PolicyError> {
    let by_ip = EgressRequest::new(request.workspace.clone(), Host::Ip(addr.ip()), request.port);
    Ok(match proxy.policy.lookup(&by_ip, SuffixAllows::Ignore)? {
        Some(Decision::Allow {
            rule_id,
            pattern: PatternKind::Exact,
        }) => IpRule::Allow(rule_id),
        Some(
            Decision::Deny { rule_id, .. }
            | Decision::SetDeny {
                rule_id: Some(rule_id),
                ..
            },
        ) => IpRule::Deny(rule_id),
        _ => IpRule::NoRule,
    })
}

/// The refusal when every address of an allowed name is denied by its IP rule (R-27). It is a
/// deny, not pending: approving the name can't change it, so no pending row is written.
fn ip_denied(
    request: &EgressRequest,
    denies: &[IpRuleHit],
    event: &mut ConnectionEvent,
) -> Refusal {
    let host = &request.host;
    let list = denies
        .iter()
        .map(|(addr, rule_id)| format!("{} (rule {rule_id})", addr.ip()))
        .collect::<Vec<_>>()
        .join(", ");
    tracing::info!(workspace = %request.workspace, %host, port = request.port, addresses = %list, "every address denied by its IP rule");
    let first = denies.first();
    event.decision = ConnectionDecision::Deny;
    event.reason = ConnectionReason::Rule;
    event.rule_id = first.map(|(_, rule_id)| *rule_id);
    event.pending_id = None;
    event.resolved_ip = first.map(|(addr, _)| addr.ip());
    let refusal = Refusal::new(
        "403 Forbidden",
        format!("{host} resolves only to addresses denied by a rule: {list}"),
    )
    .header("x-puddle-decision", "deny");
    match first {
        Some((_, rule_id)) => refusal.header("x-puddle-rule", rule_id.to_string()),
        None => refusal,
    }
}

/// The refusal when the rules could not be checked.
fn unavailable() -> Refusal {
    Refusal::new(
        "503 Service Unavailable",
        "the rules could not be checked; the connection was refused",
    )
}

/// Adds to a pending refusal why the wildcard allow didn't count.
fn wildcard_note(mut refusal: Refusal, categories: &[LocalCategory]) -> Refusal {
    if let Some(category) = categories.first() {
        refusal.message = format!(
            "{} (it resolves to a {} address, and wildcard rules don't reach local addresses: approve the exact name, add an exact rule for its address, or turn on \"wildcards reach local addresses\")",
            refusal.message,
            category.describe()
        );
    }
    refusal
}

/// The pattern kind of an allow, or the refusal for anything else.
fn allowed(
    request: &EgressRequest,
    decision: Result<Decision, PolicyError>,
) -> Result<PatternKind, Refusal> {
    let host = &request.host;
    let port = request.port;
    let workspace = &request.workspace;
    match decision {
        Ok(Decision::Allow { rule_id, pattern }) => {
            tracing::info!(%workspace, %host, port, rule = %rule_id, "allowed");
            Ok(pattern)
        }
        Ok(Decision::Deny { rule_id, .. }) => {
            tracing::info!(%workspace, %host, port, rule = %rule_id, "denied by rule");
            Err(Refusal::new(
                "403 Forbidden",
                format!("{host} is denied by a rule (rule {rule_id})"),
            )
            .header("x-puddle-decision", "deny")
            .header("x-puddle-rule", rule_id.to_string()))
        }
        // A set's allow counts like a wildcard allow for local destinations (R-42).
        Ok(Decision::SetAllow { set, .. }) => {
            tracing::info!(%workspace, %host, port, %set, "allowed by a rule set");
            Ok(PatternKind::Suffix)
        }
        Ok(Decision::SetDeny { set, rule_id, .. }) => {
            tracing::info!(%workspace, %host, port, %set, rule = ?rule_id, "denied by a rule set");
            let refusal = match rule_id {
                Some(rule_id) => Refusal::new(
                    "403 Forbidden",
                    format!(
                        "{host} is denied by rule {rule_id} of your rule set {set} (listed under Rules, Rule sets); delete that entry, switch the set off for this workspace, or add an exact allow of your own"
                    ),
                )
                .header("x-puddle-rule", rule_id.to_string()),
                None => Refusal::new(
                    "403 Forbidden",
                    format!(
                        "{host} is on the built-in rule set {set} (listed under Rules, Rule sets), which blocks it; switch the set off for this workspace, or add an exact allow of your own"
                    ),
                ),
            };
            Err(refusal
                .header("x-puddle-decision", "deny")
                .header("x-puddle-rule-set", set.to_string()))
        }
        Ok(Decision::Pending(outcome)) => {
            tracing::info!(%workspace, %host, port, pending = ?outcome.pending_id(), "pending approval");
            let refusal = Refusal::new(
                "403 Forbidden",
                match outcome {
                    PendingOutcome::Unlisted => format!(
                        "{host} is not listed for approval in Puddle yet: this workspace already has more waiting requests than Puddle lists. Answer some of them in Puddle, then retry and this one is listed"
                    ),
                    PendingOutcome::New(_) | PendingOutcome::Repeat(_) => format!(
                        "{host} is waiting for approval in Puddle; approve it there, then retry"
                    ),
                },
            )
            .header("x-puddle-decision", "pending");
            Err(match outcome.pending_id() {
                Some(id) => refusal.header("x-puddle-pending", id.to_string()),
                None => refusal,
            })
        }
        Ok(Decision::Blocked { reason }) => Err(block(request, &[reason])),
        Ok(other) => {
            tracing::warn!(%workspace, %host, port, decision = ?other, "unknown decision, refused");
            Err(
                Refusal::new("403 Forbidden", format!("{host} is not allowed"))
                    .header("x-puddle-decision", "deny"),
            )
        }
        Err(err) => {
            tracing::error!(%workspace, %host, port, error = %err, "policy unavailable, refused");
            Err(unavailable())
        }
    }
}

/// The refusal for a blocked request; `reasons` holds every address's reason (at least one).
/// The header names the first toggle if any (turning it on would help), else the first reason.
fn block(request: &EgressRequest, reasons: &[BlockReason]) -> Refusal {
    block_at(request, reasons, None)
}

/// [`block`] for a block at an address of the destination: a company-network refusal names it.
fn block_at(request: &EgressRequest, reasons: &[BlockReason], ip: Option<IpAddr>) -> Refusal {
    let host = &request.host;
    let reason = shown_reason(reasons);
    tracing::info!(workspace = %request.workspace, %host, port = request.port, %reason, "blocked");
    let message = match reason {
        BlockReason::CompanyNetwork(signal) => company_network_message(host, ip, Some(signal)),
        BlockReason::SshUnsupported
        | BlockReason::UdpUnsupported
        | BlockReason::PuddleEndpoint
        | BlockReason::LocalAddress
        | BlockReason::LocalToggle(_)
        | BlockReason::SettingsUnreadable => block_message(host, &request.workspace, reasons),
    };
    Refusal::new("403 Forbidden", message)
        .header("x-puddle-decision", "blocked")
        .header("x-puddle-blocked", reason.code())
}

/// The reason a block names: the first toggle or unreadable-settings reason if any (turning the
/// toggle on or fixing the settings would help), else the first.
fn shown_reason(reasons: &[BlockReason]) -> BlockReason {
    reasons
        .iter()
        .find(|r| {
            matches!(
                r,
                BlockReason::LocalToggle(_)
                    | BlockReason::CompanyNetwork(_)
                    | BlockReason::SettingsUnreadable
            )
        })
        .or_else(|| reasons.first())
        .copied()
        .unwrap_or(BlockReason::LocalAddress)
}

pub(crate) use puddle_upstream::connect_first;

/// What a [`tunnel`] saw.
#[derive(Debug, Default)]
pub(crate) struct TunnelEnd {
    /// The tunnel's first request line, if it carried plain HTTP/1.x.
    pub(crate) http: Option<HttpRequestLine>,
    /// The guest started SSH: nothing of it was passed on, and the guest got the refusal.
    pub(crate) ssh: bool,
    /// The guest's `ClientHello` asked for a token endpoint of the workspace's logins: it was not
    /// passed on, and the guest got a TLS alert.
    pub(crate) token_endpoint: bool,
}

/// `CONNECT`: answer `200` (unless `guest` was answered already), pass on bytes the guest sent
/// early, splice. On an error both ends reset: [`splice`] sets zero linger on the server socket,
/// and the guest stream is dropped without a shutdown.
///
/// Reports the tunnel's first request line if it carried plain HTTP/1.x (Node `fetch` and Yarn
/// Berry tunnel `http://` this way), for the audit. The bytes are relayed unchanged.
///
/// With `ssh_for` (the tunnel's request), a tunnel whose first upload bytes are an SSH
/// identification line is ended instead: the line is not passed on, the guest gets the SSH
/// refusal and a clean close, and the server a reset ([`ssh::Gate`]). That is how SSH to a host a
/// rule allows is still refused, on any port.
pub(crate) async fn tunnel<S: AsyncRead + AsyncWrite + Unpin>(
    guest: Opened<S>,
    mut server: TcpStream,
    ssh_for: Option<&EgressRequest>,
    mut watch: Option<RouteWatch>,
    token_endpoints: Option<Arc<terminate::Termination>>,
    kex: Option<crate::ssh_agent::KexWatch>,
) -> TunnelEnd {
    let Opened {
        stream,
        early,
        answered,
    } = guest;
    let tapped = TokenGate::new(RequestTap::new(stream, &early), token_endpoints, &early);
    // The gate hands `early` on first, so it reaches the server through the splice, after the
    // gate has looked at it.
    let mut guest = ssh::Gate::new(tapped, early, ssh_for.is_some()).with_kex(kex);
    if guest.inner().refused() {
        abort(&server);
        return refuse_token_endpoint(&mut guest).await;
    }
    if !answered && let Err(err) = guest.write_all(ESTABLISHED).await {
        tracing::debug!(error = %err, "tunnel failed before the splice");
        abort(&server);
        return seen_only(&guest);
    }
    // A tunnel can outlive the network it was made on. When the route to its destination changes
    // so that the hop it uses is no longer on it, both ends are reset (the same ending as any
    // failed tunnel), and the guest's client reconnects over the new route.
    let spliced = splice_watched(&mut server, &mut guest, watch.as_mut()).await;
    let Some(spliced) = spliced else {
        abort(&server);
        return seen_only(&guest);
    };
    match spliced {
        Ok((up, down)) => tracing::debug!(bytes_up = up, bytes_down = down, "tunnel closed"),
        Err(err) => {
            if let Some(request) = ssh_for.filter(|_| guest.ssh_seen()) {
                tracing::info!(host = %request.host, port = request.port, "ssh in a tunnel refused");
                let message = block_message(
                    &request.host,
                    &request.workspace,
                    &[BlockReason::SshUnsupported],
                );
                let _ = guest.write_all(&ssh::pre_banner(&message)).await;
                let _ = guest.shutdown().await;
                return TunnelEnd {
                    ssh: true,
                    ..TunnelEnd::default()
                };
            }
            if guest.inner().refused() {
                return refuse_token_endpoint(&mut guest).await;
            }
            tracing::debug!(error = %err, "tunnel aborted");
        }
    }
    seen_only(&guest)
}

/// What a tunnel that ended without a refusal saw: its first request line, if it carried plain HTTP.
fn seen_only<S>(guest: &ssh::Gate<TokenGate<RequestTap<S>>>) -> TunnelEnd {
    TunnelEnd {
        http: guest.inner().inner().request_line(),
        ..TunnelEnd::default()
    }
}

/// Ends a tunnel whose hello asked for a token endpoint: a TLS alert to the guest, a clean close.
async fn refuse_token_endpoint<S: AsyncRead + AsyncWrite + Unpin>(
    guest: &mut ssh::Gate<TokenGate<S>>,
) -> TunnelEnd {
    tracing::info!("tunnel refused: the TLS hello asks for a token endpoint");
    let _ = guest.write_all(&token_gate::ALERT).await;
    let _ = guest.shutdown().await;
    TunnelEnd {
        token_endpoint: true,
        ..TunnelEnd::default()
    }
}

/// [`splice`] until either side ends, or until `watch` says the route to the destination
/// changed (`None`: both ends are to be reset by the caller).
pub(crate) async fn splice_watched<S>(
    server: &mut TcpStream,
    guest: &mut S,
    watch: Option<&mut RouteWatch>,
) -> Option<io::Result<(u64, u64)>>
where
    S: AsyncRead + AsyncWrite + Unpin + ?Sized,
{
    let mut spliced = std::pin::pin!(splice(server, guest));
    match watch {
        Some(watch) => tokio::select! {
            result = &mut spliced => Some(result),
            () = watch.changed() => None,
        },
        None => Some(spliced.await),
    }
}

/// One plain-HTTP request on a connection that carries only it (the image-pull listener's way;
/// a workspace's plain HTTP keeps its connection, see [`plain::serve`]): `Host` rewritten to the
/// checked target, body framed exactly, so a second request can't ride on the checked connection.
pub(crate) async fn forward<S: AsyncRead + AsyncWrite + Unpin>(
    mut reader: ClientReader<S>,
    mut server: TcpStream,
    head: &Head,
    path: &str,
    target: &Target,
    body: Body,
    via: Option<&ProxyForm>,
) {
    // To a proxy the request target is the absolute URI (of the name or the checked address the
    // proxy was told), with the proxy credential for this request if the scheme needs one.
    let uri = via.map_or_else(
        || path.to_owned(),
        |proxy| format!("http://{}{path}", proxy.authority),
    );
    let authorization = via.and_then(|p| {
        p.authorization
            .as_ref()
            .map(puddle_upstream::Secret::expose)
    });
    let upstream_head = http::upstream_head(head, &uri, &target.host_header(), authorization);
    let result = async {
        server.write_all(upstream_head.as_bytes()).await?;
        forward_one_request(&mut reader, &mut server, body).await
    }
    .await;
    match result {
        Ok(0) => tracing::debug!("request forwarded"),
        Ok(extra) => tracing::debug!(
            discarded_bytes = extra,
            "request forwarded; pipelined bytes discarded"
        ),
        Err(err) => {
            tracing::debug!(error = %err, "plain-HTTP request aborted");
            abort(&server);
            // `reader` is dropped without a shutdown: the guest stream resets.
        }
    }
}

/// Sends the body up while the response comes down (so `Expect: 100-continue` works). Anything
/// the guest sends after the body is never forwarded; returns how many such bytes were buffered.
async fn forward_one_request<C, U>(
    client: &mut BufReader<C>,
    upstream: &mut U,
    body: Body,
) -> io::Result<usize>
where
    C: AsyncRead + AsyncWrite + Unpin,
    U: AsyncRead + AsyncWrite + Unpin,
{
    let (mut server_in, mut server_out) = tokio::io::split(upstream);
    let leftover = {
        let (guest_in, mut guest_out) = tokio::io::split(&mut *client);
        let mut guest_in = BufReader::new(guest_in);
        {
            let upload = async {
                http::copy_body(&mut guest_in, &mut server_out, body).await?;
                server_out.flush().await
            };
            let download = tokio::io::copy(&mut server_in, &mut guest_out);
            tokio::pin!(download);
            let uploaded_first = tokio::select! {
                up = upload => { up?; true }
                down = &mut download => { down?; false }
            };
            if uploaded_first {
                download.await?;
            }
        }
        guest_out.shutdown().await?;
        guest_in.buffer().len()
    };
    Ok(leftover + client.buffer().len())
}

/// Makes dropping `server` send a reset instead of a clean close.
pub(crate) fn abort(server: &TcpStream) {
    if let Err(err) = server.set_zero_linger() {
        tracing::debug!(error = %err, "zero linger");
    }
}

#[cfg(test)]
mod tests;
