// SPDX-License-Identifier: AGPL-3.0-or-later
//! The image-pull proxy (interim design): the listener Puddle's own registry traffic
//! goes through, so image pulls take the same way out as everything else.
//!
//! msb's registry client (run inside Puddle by the msb adapter) is given this listener as its
//! proxy, with a per-run token as the URL's userinfo (`http://puddle:<token>@127.0.0.1:<port>`),
//! through the SDK's own setting (`puddle_compute_msb::MsbConfig::with_registry_proxy`),
//! not through `HTTPS_PROXY`: the token is never in the process environment, so no child process
//! inherits it. Then:
//!
//! - **Only Puddle gets in.** The listener binds `127.0.0.1` only, and every request must carry
//!   `Proxy-Authorization: Basic` for `puddle:<token>`; anything else is a `407` and the
//!   connection is closed. The token is 256 random bits, compared in constant time, and never
//!   logged (`Debug` and `Display` redact it; the log lines carry host and port only).
//! - **Guests never reach it.** It registers itself in [`PuddleEndpoints`] as
//!   [`EndpointKind::PullProxy`], so the workspace proxy's guard blocks it.
//! - **No rules, but the guard.** Pulls are Puddle's own requests, not a workspace's, so the rules
//!   engine isn't asked and no pending row is written. The address guard still
//!   applies: Puddle's own endpoints (this listener included), cloud metadata, link-local and
//!   special addresses are blocked. Private and loopback addresses are allowed by default, so a
//!   company registry on an internal address and a developer's `localhost:5000` registry work
//!   ([`PullProxy::with_local_access`] changes that).
//! - **Same relay as the workspace proxy.** `CONNECT` is spliced (TLS stays end to end, so the
//!   registry client verifies the certificate itself: an intercepting company proxy needs its
//!   root passed to the client), and absolute-form `http://` is forwarded once with the proxy
//!   credentials dropped.
//!
//! - **Recorded in the audit.** With a log set ([`PullProxy::with_connection_log`]), every pull
//!   whose destination was parsed ends as one `connection` record with `origin: puddle` and no
//!   workspace: allowed, refused by the address guard, or failed to connect. It carries the
//!   upstream hop and the bytes, and never a header, credential or query string.
//!
//! Pulls leave through the company proxy route when one is set ([`PullProxy::with_upstream`]),
//! and connect directly otherwise.

use std::fmt::{self, Write as _};
use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use puddle_netpolicy::{
    AddressVerdict, CompanySignals, EndpointKind, LocalAccess, NetPolicy, PuddleEndpoints,
    Registration,
};
use puddle_types::{
    BlockReason, ConnectionDecision, ConnectionEvent, ConnectionLog, ConnectionOutcome,
    ConnectionReason, Host, HttpRequestLine, LocalCategory, NullConnectionLog,
};
use subtle::ConstantTimeEq;
use tokio::io::BufReader;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;
use tokio::task::{JoinHandle, JoinSet};
use tracing::Instrument;

use crate::coalesce::Coalescing;
use crate::counted::Counted;
use crate::destination::{Resolver, SystemResolver};
use crate::http::{self, Head};
use crate::proxy::{
    ProxyConfig, Refusal, Unresolved, forward, parse_request, read_request, refuse, tunnel,
};
use crate::target::Target;
use crate::upstream::{Admitted, Upstream, connect_out};

/// Random bytes in a token (256 bits).
const TOKEN_BYTES: usize = 32;

/// The user name in the proxy URL. Any other name is refused.
const USER: &str = "puddle";

/// Pause after an accept error, so a broken listener can't spin a core.
const ACCEPT_BACKOFF: Duration = Duration::from_millis(100);

/// What a pull may reach besides public addresses, unless [`PullProxy::with_local_access`] says
/// otherwise: private networks and the company network (a company registry) and loopback (a
/// local registry). Puddle's own
/// endpoints stay blocked whatever this says.
#[must_use]
pub fn default_pull_access() -> LocalAccess {
    LocalAccess::NONE
        .with_toggle(LocalCategory::Private, true)
        .with_toggle(LocalCategory::CompanyNetwork, true)
        .with_toggle(LocalCategory::Loopback, true)
}

/// The per-run token of the pull proxy: 32 random bytes as 64 lower-case hex characters.
///
/// `Debug` never shows it and there is no `Display`; [`PullToken::expose`] is the one way to
/// read it.
#[derive(Clone)]
pub struct PullToken(String);

impl PullToken {
    /// A fresh token from the OS random source.
    ///
    /// # Errors
    ///
    /// When the OS has no randomness to give.
    pub fn generate() -> io::Result<Self> {
        let mut bytes = zeroize::Zeroizing::new([0u8; TOKEN_BYTES]);
        getrandom::fill(&mut *bytes).map_err(|err| io::Error::other(err.to_string()))?;
        let text = bytes
            .iter()
            .fold(String::with_capacity(TOKEN_BYTES * 2), |mut text, b| {
                let _ = write!(text, "{b:02x}");
                text
            });
        Ok(Self(text))
    }

    /// The token text.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// The `Proxy-Authorization: Basic` credentials a client sends for this token.
    fn basic_credentials(&self) -> String {
        let plain = zeroize::Zeroizing::new(format!("{USER}:{}", self.0));
        BASE64.encode(plain.as_bytes())
    }
}

impl Drop for PullToken {
    fn drop(&mut self) {
        zeroize::Zeroize::zeroize(&mut self.0);
    }
}

impl fmt::Debug for PullToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PullToken(<redacted>)")
    }
}

/// The proxy URL for the registry client, `http://puddle:<token>@127.0.0.1:<port>`.
///
/// `Debug` and `Display` show it with the token replaced by `<redacted>`;
/// [`ProxyUrl::expose`] gives the real URL, to put into the environment.
#[derive(Clone, PartialEq, Eq)]
pub struct ProxyUrl {
    url: String,
    addr: SocketAddr,
}

impl ProxyUrl {
    /// The URL with the token, for `HTTPS_PROXY` / `HTTP_PROXY`.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.url
    }

    /// The listener's address.
    #[must_use]
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }
}

impl fmt::Display for ProxyUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "http://{USER}:<redacted>@{}", self.addr)
    }
}

impl fmt::Debug for ProxyUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ProxyUrl({self})")
    }
}

/// The image-pull proxy, bound but not serving yet: `127.0.0.1` only, a per-run token
/// every request must carry (`407` otherwise), registered in [`PuddleEndpoints`] so guests never
/// reach it, and the address guard without the rules.
///
/// Bind it early (it uses a std listener, so no async runtime is needed), put
/// [`PullProxy::proxy_url`] into the environment before any thread starts, then
/// [`PullProxy::serve`] it inside the runtime.
pub struct PullProxy {
    listener: std::net::TcpListener,
    addr: SocketAddr,
    token: PullToken,
    guard: NetPolicy,
    access: LocalAccess,
    resolver: Arc<dyn Resolver>,
    config: ProxyConfig,
    upstream: Option<Upstream>,
    log: Arc<dyn ConnectionLog>,
    registration: Registration,
}

impl fmt::Debug for PullProxy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PullProxy")
            .field("addr", &self.addr)
            .field("token", &self.token)
            .field("access", &self.access)
            .finish_non_exhaustive()
    }
}

impl PullProxy {
    /// Binds `127.0.0.1` on a free port, makes a fresh token, and registers the listener in
    /// `endpoints` (the registry the workspace proxy's guard uses), so no guest can reach it.
    ///
    /// # Errors
    ///
    /// When the port can't be bound or the OS has no randomness.
    pub fn bind(endpoints: &PuddleEndpoints) -> io::Result<Self> {
        let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let addr = listener.local_addr()?;
        let token = PullToken::generate()?;
        let registration = endpoints.register(addr, EndpointKind::PullProxy);
        let access = default_pull_access();
        Ok(Self {
            listener,
            addr,
            token,
            guard: NetPolicy::new(Arc::new(access)).with_endpoints(endpoints.clone()),
            access,
            resolver: Arc::new(Coalescing::new(SystemResolver)),
            config: ProxyConfig::default(),
            upstream: None,
            log: Arc::new(NullConnectionLog),
            registration,
        })
    }

    /// Uses `resolver` for registry names (tests point names at local servers).
    #[must_use]
    pub fn with_resolver(mut self, resolver: Arc<dyn Resolver>) -> Self {
        self.resolver = resolver;
        self
    }

    /// Which local address categories pulls may reach. Puddle's own endpoints are blocked
    /// whatever this says.
    #[must_use]
    pub fn with_local_access(mut self, access: LocalAccess) -> Self {
        self.access = access;
        self
    }

    /// Reads the company-network signals from `signals`, as the workspace proxy's guard does, so
    /// a pull to a company-network address answers to the company-network toggle. Without it the
    /// category is empty.
    #[must_use]
    pub fn with_company_signals(mut self, signals: Arc<dyn CompanySignals>) -> Self {
        self.guard = self.guard.with_company_signals(signals);
        self
    }

    /// Uses `config`'s head timeout, resolve and connect timeouts, and its
    /// `max_streams_per_workspace` as the cap on open pull connections.
    #[must_use]
    pub fn with_config(mut self, config: ProxyConfig) -> Self {
        self.config = config;
        self
    }

    /// Sends pulls out along the company proxy route. The guard still runs on every
    /// address first, and a `DIRECT` hop connects only to an address that passed.
    #[must_use]
    pub fn with_upstream(mut self, upstream: Upstream) -> Self {
        self.upstream = Some(upstream);
        self
    }

    /// Records every pull in `log`, as a `connection` record with origin `puddle`.
    #[must_use]
    pub fn with_connection_log(mut self, log: Arc<dyn ConnectionLog>) -> Self {
        self.log = log;
        self
    }

    /// Where the listener is bound (always `127.0.0.1`).
    #[must_use]
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    /// The URL for the registry client's `HTTPS_PROXY` / `HTTP_PROXY`.
    #[must_use]
    pub fn proxy_url(&self) -> ProxyUrl {
        ProxyUrl {
            url: format!("http://{USER}:{}@{}", self.token.expose(), self.addr),
            addr: self.addr,
        }
    }

    /// The token, for tests and for a client that builds its own header.
    #[must_use]
    pub fn token(&self) -> &PullToken {
        &self.token
    }

    /// Starts accepting. Must be called inside a tokio runtime.
    ///
    /// # Errors
    ///
    /// When the listener can't be handed to tokio.
    pub fn serve(self) -> io::Result<PullRoute> {
        self.listener.set_nonblocking(true)?;
        let listener = TcpListener::from_std(self.listener)?;
        let shared = Arc::new(Shared {
            credentials: self.token.basic_credentials(),
            guard: self.guard,
            access: self.access,
            resolver: self.resolver,
            streams: Arc::new(Semaphore::new(self.config.max_streams_per_workspace)),
            upstream: self.upstream,
            log: self.log,
            config: self.config,
        });
        let task = tokio::spawn(accept_loop(listener, shared));
        Ok(PullRoute {
            addr: self.addr,
            task,
            _registration: self.registration,
        })
    }
}

/// The pull proxy, serving. Dropping it (or [`PullRoute::shutdown`]) stops accepting, resets open
/// connections and removes the listener from the endpoint registry.
#[derive(Debug)]
pub struct PullRoute {
    addr: SocketAddr,
    task: JoinHandle<()>,
    _registration: Registration,
}

impl PullRoute {
    /// Where the listener is bound.
    #[must_use]
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    /// Stops the listener and waits until its connections are gone.
    pub async fn shutdown(mut self) {
        self.task.abort();
        if let Err(err) = (&mut self.task).await
            && err.is_panic()
        {
            tracing::error!("image-pull proxy task panicked");
        }
    }
}

impl Drop for PullRoute {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct Shared {
    /// The base64 credentials of `puddle:<token>`.
    credentials: String,
    guard: NetPolicy,
    access: LocalAccess,
    resolver: Arc<dyn Resolver>,
    streams: Arc<Semaphore>,
    upstream: Option<Upstream>,
    log: Arc<dyn ConnectionLog>,
    config: ProxyConfig,
}

async fn accept_loop(listener: TcpListener, shared: Arc<Shared>) {
    let mut conns = JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, peer)) => {
                    let shared = Arc::clone(&shared);
                    let span = tracing::debug_span!("pull", %peer);
                    conns.spawn(async move { serve(&shared, stream).await }.instrument(span));
                }
                Err(err) => {
                    tracing::warn!(error = %err, "image-pull proxy accept failed");
                    tokio::time::sleep(ACCEPT_BACKOFF).await;
                }
            },
            Some(_) = conns.join_next(), if !conns.is_empty() => {}
        }
    }
}

/// Whether `head` carries exactly one `Proxy-Authorization: Basic` header with `credentials`.
fn authorised(head: &Head, credentials: &str) -> bool {
    let mut values = head
        .headers
        .iter()
        .filter(|h| http::header_name(h) == "proxy-authorization")
        .map(|h| http::header_value(h));
    let (Some(value), None) = (values.next(), values.next()) else {
        return false;
    };
    let Some((scheme, presented)) = value.split_once(' ') else {
        return false;
    };
    let scheme_ok = scheme.eq_ignore_ascii_case("basic");
    let token_ok: bool = presented
        .trim()
        .as_bytes()
        .ct_eq(credentials.as_bytes())
        .into();
    scheme_ok & token_ok
}

fn unauthorised() -> Refusal {
    Refusal::new(
        "407 Proxy Authentication Required",
        "this is Puddle's image-pull proxy; only Puddle may use it",
    )
    .header("proxy-authenticate", "Basic realm=\"puddle\"")
}

async fn serve(shared: &Shared, stream: TcpStream) {
    if let Err(err) = stream.set_nodelay(true) {
        tracing::debug!(error = %err, "nodelay");
    }
    let (stream, counts) = Counted::new(stream);
    let mut reader = BufReader::new(stream);
    let Ok(_permit) = Arc::clone(&shared.streams).try_acquire_owned() else {
        let limit = shared.config.max_streams_per_workspace;
        tracing::warn!(
            limit,
            "image-pull connection refused: over the connection limit"
        );
        let refusal = Refusal::new(
            "503 Service Unavailable",
            format!("too many open image-pull connections (limit {limit})"),
        );
        refuse(reader.get_mut(), &refusal).await;
        return;
    };
    let head = match read_request(&mut reader, shared.config.head_timeout).await {
        Ok(head) => head,
        Err(None) => return,
        Err(Some(refusal)) => {
            refuse(reader.get_mut(), &refusal).await;
            return;
        }
    };
    if !authorised(&head, &shared.credentials) {
        tracing::warn!("image-pull proxy: request without Puddle's credentials refused");
        refuse(reader.get_mut(), &unauthorised()).await;
        return;
    }
    let (target, path, body) = match parse_request(&head) {
        Ok(parsed) => parsed,
        Err(refusal) => {
            refuse(reader.get_mut(), &refusal).await;
            return;
        }
    };
    let mut event = ConnectionEvent::puddle(
        target.host.clone(),
        target.port,
        ConnectionDecision::Allow,
        ConnectionReason::PuddleRequest,
    );
    if let Some(path) = &path {
        event.http = Some(HttpRequestLine::new(head.method.as_str(), path));
    }
    relay(shared, reader, &head, &target, path, body, &mut event).await;
    event.bytes_up = counts.read();
    event.bytes_down = counts.written();
    tracing::debug!(
        host = %target.host,
        bytes_up = event.bytes_up,
        bytes_down = event.bytes_down,
        "image-pull connection closed"
    );
    let log = Arc::clone(&shared.log);
    if tokio::task::spawn_blocking(move || log.record(&event))
        .await
        .is_err()
    {
        tracing::warn!("connection log panicked; record lost");
    }
}

/// Admits, connects and relays one pull, noting in `event` what happened.
async fn relay(
    shared: &Shared,
    mut reader: BufReader<Counted<TcpStream>>,
    head: &Head,
    target: &Target,
    path: Option<String>,
    body: http::Body,
    event: &mut ConnectionEvent,
) {
    let admitted = match admit(shared, target, event).await {
        Ok(admitted) => admitted,
        Err(refusal) => {
            note_failure(event, &refusal);
            refuse(reader.get_mut(), &refusal).await;
            return;
        }
    };
    let out = match connect_out(
        shared.upstream.as_ref(),
        target,
        path.is_none(),
        &admitted,
        shared.config.connect_timeout,
    )
    .await
    {
        Ok(out) => out,
        Err(refusal) => {
            note_failure(event, &refusal);
            refuse(reader.get_mut(), &refusal).await;
            return;
        }
    };
    event.outcome = Some(ConnectionOutcome::Connected);
    event.resolved_ip = out.addr.map(|addr| addr.ip());
    event.upstream.clone_from(&out.hop);
    tracing::info!(host = %target.host, port = target.port, addr = ?out.addr, hop = ?out.hop, "image-pull connection");
    match path {
        None => {
            tunnel(reader.into(), out.stream, None, out.watch, None, None).await;
        }
        Some(path) => {
            forward(
                reader,
                out.stream,
                head,
                &path,
                target,
                body,
                out.via.as_ref(),
            )
            .await;
        }
    }
}

/// Marks a pull that Puddle allowed but could not carry out (no address, nothing to connect to,
/// a company proxy that refused): the record says it failed instead of reading as a pull that
/// went through. A refusal that already decided the record (a blocked address) is left alone.
fn note_failure(event: &mut ConnectionEvent, refusal: &Refusal) {
    if event.decision != ConnectionDecision::Allow {
        return;
    }
    let code = if refusal.outcome == Some(ConnectionOutcome::Timeout) {
        "timeout"
    } else {
        "unreachable"
    };
    event.reason = ConnectionReason::PuddleRequestFailed(code);
    event.outcome = refusal.outcome;
}

/// The verdict on one address a pull would connect to, reached by `name`: the workspace
/// proxy's own verdict (`NetPolicy`), with the one difference a pull has. It has no rules, so an
/// address whose toggle is on and that a workspace would need an exact allow for is let through.
fn verdict(
    guard: &NetPolicy,
    access: LocalAccess,
    name: Option<&Host>,
    addr: SocketAddr,
) -> Result<(), BlockReason> {
    match guard.check_address_with(access, name, addr) {
        AddressVerdict::Allow | AddressVerdict::ExactOnly(_) => Ok(()),
        AddressVerdict::Block(reason) => Err(reason),
    }
}

/// The addresses a pull to `target` may connect to: the name stage (`localhost`, metadata
/// names, Puddle's own names, `localhost:<puddle port>`), then every resolved address through
/// [`verdict`].
async fn admit(
    shared: &Shared,
    target: &Target,
    event: &mut ConnectionEvent,
) -> Result<Admitted, Refusal> {
    let host = &target.host;
    let port = target.port;
    let named = puddle_netpolicy::Target::from_host(host.clone());
    if let Some(reason) = shared.guard.check_target_with(shared.access, &named, port) {
        return Err(blocked(host, port, &[reason], event));
    }
    let addrs = match host {
        Host::Ip(ip) => vec![SocketAddr::new(*ip, port)],
        Host::Name(name) => {
            match tokio::time::timeout(
                shared.config.resolve_timeout,
                shared.resolver.resolve(name, port),
            )
            .await
            {
                Err(_) => {
                    let (why, refusal) = Unresolved::TimedOut.refusal(host);
                    return left_to_upstream(shared, host, why).ok_or(refusal);
                }
                Ok(Err(err)) => {
                    tracing::info!(%host, error = %err, "image-pull resolve failed");
                    let (why, refusal) = Unresolved::from_error(&err).refusal(host);
                    return left_to_upstream(shared, host, why).ok_or(refusal);
                }
                Ok(Ok(addrs)) => addrs,
            }
        }
    };
    let resolved = addrs.len();
    let mut usable = Vec::new();
    let mut reasons = Vec::new();
    for addr in addrs {
        match verdict(&shared.guard, shared.access, Some(host), addr) {
            Ok(()) => usable.push(addr),
            Err(reason) => reasons.push(reason),
        }
    }
    if usable.is_empty() {
        if reasons.is_empty() {
            return Err(
                Refusal::new("502 Bad Gateway", format!("{host} has no addresses"))
                    .failed(ConnectionOutcome::ConnectFailed),
            );
        }
        return Err(blocked(host, port, &reasons, event));
    }
    // A proxy may be told the name only when every address it resolved to passed the guard.
    let name_ok = matches!(host, Host::Ip(_)) || usable.len() == resolved;
    Ok(Admitted::checked(usable, name_ok))
}

/// A registry name this host cannot resolve, when the company proxy may resolve it (networks
/// without external DNS): there is no address to guard, and the name stage already ran.
fn left_to_upstream(shared: &Shared, host: &Host, why: &str) -> Option<Admitted> {
    if shared
        .upstream
        .as_ref()
        .is_some_and(Upstream::resolves_unknown_names)
    {
        tracing::info!(%host, why, "image pull: name not resolved here; the company proxy resolves it");
        Some(Admitted::unresolved())
    } else {
        None
    }
}

/// The refusal for a pull to an address it may not reach.
fn blocked(
    host: &Host,
    port: u16,
    reasons: &[BlockReason],
    event: &mut ConnectionEvent,
) -> Refusal {
    let reason = reasons
        .iter()
        .find(|r| matches!(r, BlockReason::PuddleEndpoint))
        .or_else(|| reasons.first())
        .copied()
        .unwrap_or(BlockReason::LocalAddress);
    event.decision = ConnectionDecision::Blocked;
    event.reason = ConnectionReason::Blocked(reason);
    tracing::info!(%host, port, %reason, "image pull blocked");
    let toggle = match reason {
        BlockReason::LocalToggle(category) => Some(category),
        BlockReason::CompanyNetwork(_) => Some(LocalCategory::CompanyNetwork),
        BlockReason::SshUnsupported
        | BlockReason::UdpUnsupported
        | BlockReason::PuddleEndpoint
        | BlockReason::LocalAddress
        | BlockReason::SettingsUnreadable => None,
    };
    let what = match (reason, toggle) {
        (BlockReason::PuddleEndpoint, _) => "one of Puddle's own endpoints".to_owned(),
        (_, Some(category)) => format!("a {} address", category.describe()),
        _ => "an address image pulls may not reach".to_owned(),
    };
    // A switch can lift the refusal, so the text names it instead of saying "never".
    let ending = toggle.map_or_else(
        || "image pulls never go there".to_owned(),
        |category| {
            format!(
                "image pulls reach it only while the '{}' switch is on in Settings",
                category.key()
            )
        },
    );
    Refusal::new("403 Forbidden", format!("{host} is {what}; {ending}"))
        .header("x-puddle-decision", "blocked")
        .header("x-puddle-blocked", reason.code())
}

#[cfg(test)]
mod tests;
