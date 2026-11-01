// SPDX-License-Identifier: GPL-3.0-or-later
//! The host side of one agent connection: accept the yamux session, serve the control stream,
//! hand every other stream to the proxy.
//!
//! ```no_run
//! # use std::sync::Arc;
//! # use puddle_agent_proto::host::{serve_session, GuestStream, HostConfig, StreamHandler};
//! # use puddle_types::{NullSink, WorkspaceName};
//! struct Proxy;
//! impl StreamHandler for Proxy {
//!     async fn handle(&self, stream: GuestStream) {
//!         // read the CONNECT line from `stream`, decide, splice ...
//!         # drop(stream);
//!     }
//! }
//! # async fn f(
//! #     conn: impl tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
//! # ) -> Result<(), Box<dyn std::error::Error>> {
//! let workspace = WorkspaceName::new("box")?;
//! serve_session(conn, workspace, Arc::new(NullSink), Arc::new(Proxy), HostConfig::default()).await?;
//! # Ok(()) }
//! ```

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use futures_util::StreamExt;
use puddle_types::{Event, EventSink, WorkspaceName};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader, ReadBuf};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tokio::time::Instant;
use tokio_yamux::{Session, StreamHandle};

use crate::control::{
    self, AgentMessage, EditorProblemKind, MAX_EDITOR_PROBLEM_CHARS, MAX_LINE, MAX_PORTS,
    PROTOCOL_VERSION, StubDnsTrouble,
};
use crate::forward::GuestLink;
use crate::kind::{MAX_PREAMBLE, StreamKind, parse_preamble};
use crate::resolve::{self, ResolveAnswer, ResolveQuery};
use crate::yamux::{VERSION_BYTE, server_config};

/// Longest reason text taken from a capture-unavailable report.
const MAX_REASON_CHARS: usize = 200;

/// Longest destination address text taken from a UDP refusal report (an IPv6 address with a zone
/// is under 50).
const MAX_ADDRESS_CHARS: usize = 64;

/// Longest end-of-output text taken from an entrypoint exit report (what the boot hook's wrapper
/// writes is 300 characters at most).
pub const MAX_TAIL_CHARS: usize = 300;

/// Name used in [`Event::OomKill`] when the agent couldn't tell which process was killed (its pid
/// is then 0).
pub const UNKNOWN_PROCESS: &str = "unknown";

/// Longest agent version string kept for the log.
const MAX_VERSION_CHARS: usize = 64;

/// Limits for one session. The defaults suit a real guest; tests shorten them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostConfig {
    /// How long a new connection or stream may stay silent before its first byte. A proxied
    /// client sends its request line at once; the agent writes the control preamble at once.
    pub first_byte_timeout: Duration,
    /// Control messages a stream may send in one burst.
    pub control_burst: u32,
    /// Control messages per second a stream may send after its burst. Messages over the limit are
    /// dropped and counted in a warning.
    pub control_per_second: u32,
    /// Name lookups (`resolve` streams) one session may have open at once. Over it, a lookup is
    /// answered [`ResolveAnswer::Unavailable`] without asking the handler.
    pub max_resolves: usize,
    /// How long the handler may take to answer one lookup. Over it: [`ResolveAnswer::Unavailable`].
    pub resolve_timeout: Duration,
}

impl Default for HostConfig {
    fn default() -> Self {
        Self {
            first_byte_timeout: Duration::from_secs(30),
            control_burst: 32,
            control_per_second: 10,
            max_resolves: 64,
            resolve_timeout: Duration::from_secs(15),
        }
    }
}

/// Why a session ended other than cleanly.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SessionError {
    /// The connection doesn't start with a yamux frame, so it isn't from `puddle-agent`.
    #[error("not a yamux session: first byte {0:#04x}")]
    NotYamux(u8),
    /// Nothing arrived within [`HostConfig::first_byte_timeout`].
    #[error("no data from the guest within {0:?}")]
    Timeout(Duration),
    /// The connection or the yamux session failed.
    #[error("session i/o: {0}")]
    Io(#[from] io::Error),
}

/// What the host does with a proxied guest connection (one yamux stream that isn't the control
/// stream). The proxy implements it.
pub trait StreamHandler: Send + Sync + 'static {
    /// Serves one connection. Errors are the handler's to log; to pass an abort on, drop the
    /// stream without shutting it down (see [`crate::relay`]).
    fn handle(&self, stream: GuestStream) -> impl Future<Output = ()> + Send;

    /// Answers one name lookup from the guest's stub DNS. The default says
    /// [`ResolveAnswer::Unavailable`]: a handler without a name policy never invents an answer.
    /// The query is untrusted; the handler normalises and checks it.
    fn resolve(&self, query: ResolveQuery) -> impl Future<Output = ResolveAnswer> + Send {
        let _ = query;
        async { ResolveAnswer::Unavailable }
    }

    /// Records that the guest's capture rules refused UDP. The default does nothing: a handler
    /// without an audit has nowhere to put it. The report is untrusted.
    fn udp_refused(&self, refusal: UdpRefusal) -> impl Future<Output = ()> + Send {
        let _ = refusal;
        async {}
    }

    /// Hands the handler the way to open streams to this session's agent, as soon as the agent has
    /// said hello. A workspace has several sessions; this is the one that carries the agent's
    /// control stream, and a newer call replaces an older one. The default drops it: a handler
    /// that never reaches into the guest needs none.
    fn guest_link(&self, link: GuestLink) -> impl Future<Output = ()> + Send {
        let _ = link;
        async {}
    }

    /// Tells the handler which TCP ports the guest listens on now (the whole set, ascending, at
    /// most [`MAX_PORTS`]). The agent's claim: it can be wrong or hostile, so nothing may be
    /// trusted beyond "a port number". The default does nothing.
    fn guest_ports(&self, ports: Vec<u16>) -> impl Future<Output = ()> + Send {
        let _ = ports;
        async {}
    }

    /// Takes a report about the workspace (an entrypoint exit, stub DNS trouble, capture rules the
    /// guest tore down or that cannot be installed), already cleaned
    /// and logged by the session. The default does nothing more. The report is the guest's claim:
    /// it may become a notice for this workspace, never a decision.
    fn agent_report(&self, report: AgentReport) -> impl Future<Output = ()> + Send {
        let _ = report;
        async {}
    }
}

/// Where a host program takes the agent reports of all its workspaces. The proxy's handler passes
/// each report on with the workspace of the route that carried it. Must not block.
pub trait AgentReports: Send + Sync {
    /// Takes one report about `workspace`.
    fn report(&self, workspace: &WorkspaceName, report: AgentReport);
}

/// A report from the guest agent about its own workspace, cleaned by the session. Nothing in it
/// names the workspace: that is the route's.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum AgentReport {
    /// The image's entrypoint exited.
    EntrypointExited {
        /// Its exit status.
        status: u8,
        /// The end of its output, at most [`MAX_TAIL_CHARS`] characters, control characters
        /// replaced by `?`.
        tail: String,
    },
    /// The stub DNS turned something away or could not carry it.
    StubDns {
        /// What happened (never [`StubDnsTrouble::Unknown`]: the session drops those).
        kind: StubDnsTrouble,
        /// How often since the agent's last report of this kind.
        count: u32,
    },
    /// A problem with the browser editor's server, or the end of one.
    EditorStatus {
        /// Which problem (never [`EditorProblemKind::Unknown`]: the session drops those).
        kind: EditorProblemKind,
        /// What is wrong, at most [`MAX_EDITOR_PROBLEM_CHARS`] characters, control characters
        /// replaced by `?`; `None` when the problem of this `kind` is over.
        problem: Option<String>,
    },
    /// Something in the guest deleted or edited the capture rules; the agent put them back. One
    /// report per event: the host counts them.
    CaptureTampered {
        /// `true` when the whole table was gone, `false` when it was edited.
        table_missing: bool,
    },
    /// The capture rules cannot be installed, or can again.
    CaptureUnavailable {
        /// What the kernel or the agent said, at most `MAX_REASON_CHARS` (200) characters, control
        /// characters replaced by `?`; `None` when the rules are installed.
        reason: Option<String>,
    },
}

/// Who wants to know what a workspace's agent says about the guest. Synchronous and quick: it
/// runs on the session's control stream, so it records and returns.
pub trait GuestWatcher: Send + Sync + 'static {
    /// The agent of `workspace` is connected; `link` opens streams to it. A later call for the
    /// same workspace (the agent restarted, a session died) replaces the earlier link.
    fn link(&self, workspace: &WorkspaceName, link: GuestLink);

    /// The ports `workspace`'s guest listens on now: the whole set, ascending, no duplicates, at
    /// most [`MAX_PORTS`]. The guest's claim; nothing else is known.
    fn ports(&self, workspace: &WorkspaceName, ports: Vec<u16>);
}

/// A report from the guest that UDP was refused: the guest's own claim, cleaned by the host
/// before it is used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UdpRefusal {
    /// The destination address as the guest wrote it.
    pub address: String,
    /// The destination port.
    pub port: u16,
    /// Datagrams refused since the last report for it.
    pub count: u32,
}

/// One proxied guest connection: a yamux stream with its first byte (already read by the host to
/// tell it from the control stream) put back in front.
#[derive(Debug)]
pub struct GuestStream(Prefixed<StreamHandle>);

impl AsyncRead for GuestStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_read(cx, buf)
    }
}

impl AsyncWrite for GuestStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.0).poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_shutdown(cx)
    }
}

/// A stream with one already-read byte put back in front of it.
#[derive(Debug)]
struct Prefixed<S> {
    first: Option<u8>,
    inner: S,
}

impl<S: AsyncRead + Unpin> AsyncRead for Prefixed<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if buf.remaining() > 0
            && let Some(b) = self.first.take()
        {
            buf.put_slice(&[b]);
            return Poll::Ready(Ok(()));
        }
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Prefixed<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

/// State shared by the streams of one session.
struct Shared<H> {
    workspace: WorkspaceName,
    sink: Arc<dyn EventSink>,
    handler: Arc<H>,
    config: HostConfig,
    /// Set while a control stream is open: a session has at most one.
    control_open: AtomicBool,
    /// Opens streams to the agent on this session.
    link: GuestLink,
    /// Lookups in flight on this session.
    resolves: Semaphore,
}

/// Serves one connection from a workspace's agent until it closes. `workspace` is the workspace the
/// route belongs to (the route is the identity): nothing the guest sends can change it.
///
/// Every stream task is owned here and ends when the session does.
///
/// # Errors
///
/// [`SessionError::NotYamux`] for a connection that isn't a yamux session,
/// [`SessionError::Timeout`] if it stays silent, [`SessionError::Io`] if the session fails. A
/// clean close by the guest is `Ok`.
pub async fn serve_session<IO, H>(
    mut io: IO,
    workspace: WorkspaceName,
    sink: Arc<dyn EventSink>,
    handler: Arc<H>,
    config: HostConfig,
) -> Result<(), SessionError>
where
    IO: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    H: StreamHandler,
{
    let Some(first) = first_byte(&mut io, config.first_byte_timeout).await? else {
        return Ok(());
    };
    if first != VERSION_BYTE {
        return Err(SessionError::NotYamux(first));
    }
    let mut session = Session::new_server(
        Prefixed {
            first: Some(first),
            inner: io,
        },
        server_config(),
    );
    let shared = Arc::new(Shared {
        link: GuestLink::new(session.control()),
        workspace,
        sink,
        handler,
        resolves: Semaphore::new(config.max_resolves),
        config,
        control_open: AtomicBool::new(false),
    });
    let mut tasks = JoinSet::new();
    loop {
        tokio::select! {
            inbound = session.next() => match inbound {
                None => return Ok(()),
                Some(Err(err)) => return Err(err.into()),
                Some(Ok(stream)) => {
                    tasks.spawn(serve_stream(stream, Arc::clone(&shared)));
                }
            },
            Some(done) = tasks.join_next(), if !tasks.is_empty() => {
                if let Err(err) = done
                    && err.is_panic()
                {
                    tracing::error!(workspace = %shared.workspace, "stream task panicked");
                }
            }
        }
    }
}

/// Reads the first byte within `limit`; `None` at a clean end of stream.
async fn first_byte<R: AsyncRead + Unpin>(
    io: &mut R,
    limit: Duration,
) -> Result<Option<u8>, SessionError> {
    let mut first = [0u8; 1];
    match tokio::time::timeout(limit, io.read(&mut first)).await {
        Err(_) => Err(SessionError::Timeout(limit)),
        Ok(Err(err)) => Err(err.into()),
        Ok(Ok(0)) => Ok(None),
        Ok(Ok(_)) => Ok(Some(first[0])),
    }
}

async fn serve_stream<H: StreamHandler>(mut stream: StreamHandle, shared: Arc<Shared<H>>) {
    let first = match first_byte(&mut stream, shared.config.first_byte_timeout).await {
        Ok(Some(b)) => b,
        Ok(None) => return,
        Err(err) => {
            tracing::debug!(workspace = %shared.workspace, error = %err, "guest stream dropped before its first byte");
            return;
        }
    };
    if first != 0 {
        let stream = GuestStream(Prefixed {
            first: Some(first),
            inner: stream,
        });
        shared.handler.handle(stream).await;
        return;
    }
    let mut reader = BufReader::new(Prefixed {
        first: Some(first),
        inner: stream,
    });
    let mut line = Vec::with_capacity(MAX_PREAMBLE);
    let read = tokio::time::timeout(
        shared.config.first_byte_timeout,
        control::read_line(&mut reader, &mut line, MAX_PREAMBLE),
    )
    .await;
    if !matches!(read, Ok(Ok(true))) {
        tracing::warn!(workspace = %shared.workspace, "stream with an unreadable preamble closed");
        return;
    }
    match parse_preamble(&line) {
        Ok((StreamKind::Control, 1)) => {
            if shared.control_open.swap(true, Ordering::AcqRel) {
                tracing::warn!(workspace = %shared.workspace, "second control stream on one session refused");
                return;
            }
            serve_control(reader, &shared).await;
            shared.control_open.store(false, Ordering::Release);
        }
        Ok((StreamKind::Resolve, 1)) => serve_resolve(reader, &shared).await,
        Ok((kind, version)) => {
            tracing::warn!(workspace = %shared.workspace, kind = kind.name(), version, "stream kind not served by this host, closed");
        }
        Err(err) => {
            tracing::warn!(workspace = %shared.workspace, error = %err, "stream closed");
        }
    }
}

/// Answers one lookup: reads the query line, asks the handler (bounded in number and time), writes
/// the answer line and closes the stream cleanly. The preamble is already read.
async fn serve_resolve<H: StreamHandler>(
    mut reader: BufReader<Prefixed<StreamHandle>>,
    shared: &Shared<H>,
) {
    let workspace = &shared.workspace;
    let query = match tokio::time::timeout(
        shared.config.first_byte_timeout,
        resolve::read_query(&mut reader),
    )
    .await
    {
        Ok(Ok(query)) => query,
        Ok(Err(err)) => {
            tracing::debug!(%workspace, error = %err, "resolve stream with an unreadable query closed");
            return;
        }
        Err(_) => {
            tracing::debug!(%workspace, "resolve stream closed: no query in time");
            return;
        }
    };
    let answer = if let Ok(_permit) = shared.resolves.try_acquire() {
        if let Ok(answer) =
            tokio::time::timeout(shared.config.resolve_timeout, shared.handler.resolve(query)).await
        {
            answer
        } else {
            tracing::warn!(%workspace, timeout_secs = shared.config.resolve_timeout.as_secs(), "the name lookup handler did not answer in time; answered unavailable");
            ResolveAnswer::Unavailable
        }
    } else {
        tracing::warn!(%workspace, limit = shared.config.max_resolves, "too many lookups open on one session; answered unavailable");
        ResolveAnswer::Unavailable
    };
    let line = match answer.to_line() {
        Ok(line) => line,
        Err(err) => {
            tracing::warn!(%workspace, error = %err, "resolve answer not sent");
            return;
        }
    };
    let stream = reader.get_mut();
    if stream.write_all(&line).await.is_ok() {
        let _ = stream.shutdown().await;
    }
}

/// Reads control messages until the stream ends or misbehaves. The preamble is already read.
async fn serve_control<H: StreamHandler>(
    mut reader: BufReader<Prefixed<StreamHandle>>,
    shared: &Shared<H>,
) {
    let workspace = &shared.workspace;
    let mut limit = RateLimit::new(
        shared.config.control_burst,
        shared.config.control_per_second,
        Instant::now(),
    );
    let mut line = Vec::with_capacity(256);
    loop {
        match control::read_line(&mut reader, &mut line, MAX_LINE).await {
            Ok(true) => {}
            Ok(false) => break,
            Err(err) => {
                tracing::warn!(%workspace, error = %err, "control stream ended");
                break;
            }
        }
        if !limit.allow(Instant::now()) {
            continue;
        }
        if limit.suppressed > 0 {
            tracing::warn!(%workspace, dropped = limit.suppressed, "control messages over the rate limit were dropped");
            limit.suppressed = 0;
        }
        match AgentMessage::from_line(&line) {
            Ok(msg) => on_message(msg, shared).await,
            Err(err) => tracing::warn!(%workspace, error = %err, "invalid control message ignored"),
        }
    }
    // A flood that stops never gets a later message through to report its count.
    if limit.suppressed > 0 {
        tracing::warn!(%workspace, dropped = limit.suppressed, "control messages over the rate limit were dropped");
    }
}

async fn on_message<H: StreamHandler>(msg: AgentMessage, shared: &Shared<H>) {
    let workspace = &shared.workspace;
    match msg {
        AgentMessage::Hello {
            agent_version,
            protocol,
        } => {
            let version = clean(&agent_version, MAX_VERSION_CHARS);
            if protocol != PROTOCOL_VERSION {
                tracing::warn!(%workspace, agent_protocol = protocol, host_protocol = PROTOCOL_VERSION, "guest agent speaks another control protocol; connection kept");
            }
            tracing::info!(%workspace, agent_version = %version, protocol, "guest agent connected");
            shared.handler.guest_link(shared.link.clone()).await;
        }
        AgentMessage::Ports { mut listening } => {
            listening.retain(|p| *p != 0);
            listening.sort_unstable();
            listening.dedup();
            listening.truncate(MAX_PORTS);
            shared.handler.guest_ports(listening).await;
        }
        AgentMessage::OomKill { pid, process } => {
            let event = Event::oom_kill(
                workspace.clone(),
                pid.unwrap_or(0),
                process.as_deref().unwrap_or(UNKNOWN_PROCESS),
            );
            if let Event::OomKill { pid, process, .. } = &event {
                tracing::warn!(%workspace, pid, process = %process, "guest out of memory: process killed");
            }
            shared.sink.emit(event);
        }
        msg @ (AgentMessage::CaptureTampered { .. }
        | AgentMessage::CaptureUnavailable { .. }
        | AgentMessage::CaptureReady) => on_capture(msg, shared).await,
        AgentMessage::UdpRefused {
            address,
            port,
            count,
        } => {
            let refusal = UdpRefusal {
                address: clean(&address, MAX_ADDRESS_CHARS),
                port,
                count,
            };
            shared.handler.udp_refused(refusal).await;
        }
        AgentMessage::EntrypointExited { status, tail } => {
            let tail = clean(&tail, MAX_TAIL_CHARS);
            if status == 0 {
                tracing::info!(%workspace, "the image's entrypoint exited with status 0");
            } else {
                tracing::warn!(%workspace, status, %tail, "the image's entrypoint exited with a failure");
            }
            shared
                .handler
                .agent_report(AgentReport::EntrypointExited { status, tail })
                .await;
        }
        AgentMessage::StubDns {
            kind: StubDnsTrouble::Unknown,
            ..
        } => {
            tracing::debug!(%workspace, "stub DNS report of an unknown kind ignored");
        }
        AgentMessage::StubDns { kind, count } => {
            tracing::warn!(%workspace, ?kind, count, "the guest's stub DNS turned lookups away");
            shared
                .handler
                .agent_report(AgentReport::StubDns { kind, count })
                .await;
        }
        AgentMessage::EditorStatus { kind, problem } => {
            on_editor_status(workspace, kind, problem, shared).await;
        }
        AgentMessage::Unknown => {
            tracing::debug!(%workspace, "unknown control message ignored");
        }
    }
}

/// The three messages about the capture rules, each becoming a report.
async fn on_capture<H: StreamHandler>(msg: AgentMessage, shared: &Shared<H>) {
    let workspace = &shared.workspace;
    let report = match msg {
        AgentMessage::CaptureTampered { table_missing } => {
            tracing::warn!(%workspace, table_missing, "the guest changed the capture rules; the agent restored them");
            AgentReport::CaptureTampered { table_missing }
        }
        AgentMessage::CaptureUnavailable { reason } => {
            let mut reason = clean(&reason, MAX_REASON_CHARS);
            if reason.is_empty() {
                // `None` means "installed"; an outage always has a reason.
                reason.push_str("no reason given");
            }
            tracing::warn!(%workspace, %reason, "the guest could not install its capture rules; tools that ignore the proxy settings fail until it can");
            AgentReport::CaptureUnavailable {
                reason: Some(reason),
            }
        }
        _ => {
            tracing::info!(%workspace, "the guest's capture rules are installed");
            AgentReport::CaptureUnavailable { reason: None }
        }
    };
    shared.handler.agent_report(report).await;
}

async fn on_editor_status<H: StreamHandler>(
    workspace: &WorkspaceName,
    kind: EditorProblemKind,
    problem: Option<String>,
    shared: &Shared<H>,
) {
    if kind == EditorProblemKind::Unknown {
        tracing::debug!(%workspace, "editor report of an unknown kind ignored");
        return;
    }
    let problem = problem
        .map(|p| clean(&p, MAX_EDITOR_PROBLEM_CHARS))
        .filter(|p| !p.is_empty());
    if let Some(problem) = &problem {
        tracing::warn!(%workspace, ?kind, %problem, "the guest's editor server has a problem");
    } else {
        tracing::info!(%workspace, ?kind, "the guest's editor server has no such problem now");
    }
    shared
        .handler
        .agent_report(AgentReport::EditorStatus { kind, problem })
        .await;
}

/// Cuts `s` to `max` characters and replaces control characters, for guest text in logs.
fn clean(s: &str, max: usize) -> String {
    s.chars()
        .take(max)
        .map(|c| if c.is_control() { '?' } else { c })
        .collect()
}

/// A token bucket: `burst` messages at once, then `per_second`.
#[derive(Debug)]
struct RateLimit {
    tokens: f64,
    burst: f64,
    per_second: f64,
    last: Instant,
    /// Messages dropped since the last one that got through.
    suppressed: u64,
}

impl RateLimit {
    fn new(burst: u32, per_second: u32, now: Instant) -> Self {
        Self {
            tokens: f64::from(burst),
            burst: f64::from(burst),
            per_second: f64::from(per_second),
            last: now,
            suppressed: 0,
        }
    }

    fn allow(&mut self, now: Instant) -> bool {
        let elapsed = now.saturating_duration_since(self.last).as_secs_f64();
        self.last = now;
        self.tokens = (self.tokens + elapsed * self.per_second).min(self.burst);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            self.suppressed += 1;
            false
        }
    }
}

#[cfg(test)]
mod tests;
