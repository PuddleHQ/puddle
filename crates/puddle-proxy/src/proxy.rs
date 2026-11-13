// SPDX-License-Identifier: GPL-3.0-or-later
//! One proxied guest connection: read the request head, decide, connect, splice.
//!
//! The order matters for security: the head is bounded in size and time before anything else;
//! the rules engine decides on the normalised name **before** it is resolved (R-10); every
//! resolved address is checked against its IP rules (R-27) and its address class (R-14), and the
//! proxy connects only to an address that passed.
//! Every refusal is an HTTP error response followed by a clean close (a reset could overtake the
//! response); every failure after the connection is open is passed on as a reset.

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use puddle_agent_proto::forward::GuestLink;
use puddle_agent_proto::host::{
    AgentReport, AgentReports, AgentStream, GuestStream, GuestWatcher, StreamHandler, UdpRefusal,
};
use puddle_agent_proto::resolve::{ResolveAnswer, ResolveQuery};
use puddle_types::{
    BlockReason, ConnectionDecision, ConnectionEvent, ConnectionLog, ConnectionReason, Decision,
    EgressRequest, EventSink, Host, HttpRequestLine, NullConnectionLog, Policy, PolicyError,
    ProtocolHint, SuffixAllows, WorkspaceId,
};
use tokio::io::{AsyncBufRead, BufReader};
use tokio::sync::Semaphore;
use tracing::Instrument;

use puddle_netpolicy::{LocalAccess, NetPolicy, normalise_host};

use crate::coalesce::Coalescing;
use crate::counted::Counted;
use crate::destination::{AddressCheck, Resolver, SystemResolver};
use crate::http::{self, Body, Head, HeadError, RawTarget};
use crate::plain;
use crate::records::{RecordResolver, SystemRecords};
use crate::ssh;
use crate::target::Target;
use crate::terminate::{self, TerminationSource};
use crate::upstream::{Admitted, Upstream};

mod admit;
mod config;
mod forward;
mod refusal;
mod relay;

use admit::resolve_destination;
pub(crate) use admit::{Unresolved, admit};
pub use config::ProxyConfig;
pub(crate) use forward::{connect_first, forward, tunnel};
pub(crate) use refusal::{Refusal, refuse};
use relay::relay;

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

/// The client side of one proxied stream, counted for the audit.
pub(crate) type ClientReader<S> = BufReader<Counted<S>>;

/// The guest side of one proxied stream.
pub(super) type GuestReader = ClientReader<GuestStream>;

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

#[cfg(test)]
mod tests;
