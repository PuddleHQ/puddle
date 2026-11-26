// SPDX-License-Identifier: AGPL-3.0-or-later
//! A terminated connection whose guest speaks HTTP/2.
//!
//! Every stream is one request, decided, checked and injected exactly like an HTTP/1.1 request on
//! the strict path: the same checks (`Host`, target, framing), the same [`Injector`] and the same
//! header rules, then sent over the upstream leg, which is an HTTP/2 connection (streams
//! multiplexed) or, for a server that speaks only HTTP/1.1, a small pool of connections. Bodies
//! stream both ways with their trailers, so gRPC works.
//!
//! [`Injector`]: super::Injector

use std::convert::Infallible;
use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::task::{Context as TaskContext, Poll};
use std::time::Duration;

use ::http::header::{self, HeaderMap, HeaderName, HeaderValue};
use ::http::{Method, Request, Response, StatusCode, Uri};
use bytes::Bytes;
use http_body::{Body, Frame, SizeHint};
use http_body_util::{BodyExt as _, Full};
use hyper::body::Incoming;
use hyper::server::conn::http2;
use hyper::service::service_fn;
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use puddle_types::{LimitCode, ReasonCode};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::Notify;
use tokio::time::Instant;

use super::exchange::AnswerBody;
use super::guest::{ALPN_H2, ALPN_HTTP11};
use super::inject::{Injection, body_too_large};
use super::leg::{self, BoxError, Connected, H1Pool, H2Conn, Lease, UpBody};
use super::pipeline::{self, GuestHeaders, GuestRequest, Prepared, RequestAudit, Wire};
use super::request::{self, UpstreamVersion, bad, misdirected};
use super::session::short;
use super::session::{Context, Outcome};
use super::stand_in::{AnswerReport, ScrubNotes};
use super::streams::{
    Activity, Clock, Registry, SWEEP_INTERVAL, Step, StreamGuard, Sweeper, ended,
};
use super::watch::Watch;
use super::ws;
use crate::proxy::Refusal;

/// Streams a guest may have open at once (RFC 9113 §6.5.2). Real tools open a few dozen; the
/// bar is the hundreds the spec names for connections.
pub(crate) const MAX_CONCURRENT_STREAMS: u32 = 256;

/// The largest request header list accepted from the guest (the plain-HTTP path's head limit).
const MAX_REQUEST_HEADER_LIST: u32 = 64 * 1024;

/// Streams the guest reset before the proxy accepted them, tolerated before the connection is
/// ended (rapid reset, CVE-2023-44487).
const MAX_PENDING_ACCEPT_RESETS: usize = 20;

/// Streams the proxy reset because of the guest's protocol errors, tolerated before the
/// connection is ended.
const MAX_LOCAL_ERROR_RESETS: usize = 128;

/// The flow-control window of one stream toward the guest, and of the whole connection: how much
/// the guest may send before the proxy has read it. Fixed, not adaptive: the guest is on the same
/// machine, so the proxy's turnaround is all a window has to cover, and an adaptive window would
/// grow to 16 MiB on a connection that then stops being read.
const GUEST_STREAM_WINDOW: u32 = 512 * 1024;
const GUEST_CONNECTION_WINDOW: u32 = 2 * 1024 * 1024;

/// What an HTTP/1.1 pool checkout may wait for a free connection.
const POOL_WAIT: Duration = Duration::from_secs(30);

/// A response body or request body as the HTTP/2 server and the upstream clients take it.
type RespBody = http_body_util::combinators::UnsyncBoxBody<Bytes, BoxError>;

/// The upstream leg of one guest connection.
enum LegState {
    None,
    H2(H2Conn),
    H1(H1Pool),
}

/// Where one stream's request goes.
enum Route {
    H2(hyper::client::conn::http2::SendRequest<UpBody>),
    H1(Lease),
}

impl Route {
    fn version(&self) -> UpstreamVersion {
        match self {
            Self::H2(_) => UpstreamVersion::H2,
            Self::H1(_) => UpstreamVersion::H1,
        }
    }

    /// Nothing was sent on this route: an HTTP/1.1 connection checked out for it is still good.
    fn unused(self) {
        if let Self::H1(mut lease) = self {
            lease.mark_reusable();
        }
    }
}

struct Shared {
    cx: Arc<Context>,
    /// The upstream connection (or pool) in use. Held only to read or replace it, never across
    /// a connect.
    leg: Mutex<LegState>,
    /// One stream at a time sets the upstream connection up; the others wait here and share the
    /// outcome. The last failure is kept with the moment it happened.
    connecting: tokio::sync::Mutex<Option<(Instant, Refusal)>>,
    /// Why the real server could not be used at the handshake; the first stream gets it.
    deferred: Mutex<Option<Refusal>>,
    outcome: Mutex<Outcome>,
    /// What the scrub of the answers did; it joins the outcome when the connection ends.
    scrubbed: Arc<ScrubNotes>,
    /// The tokens the answers held that Puddle does not hold; they join the outcome too.
    unknown_tokens: Arc<AnswerReport>,
    streams: Arc<Registry>,
}

/// What the upstream leg offers a stream.
enum Usable {
    H2(hyper::client::conn::http2::SendRequest<UpBody>),
    Pool(H1Pool),
}

impl Shared {
    /// The route for one more request, connecting again when the upstream connection is gone.
    async fn route(&self) -> Result<Route, Refusal> {
        let deferred = self
            .deferred
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(refusal) = deferred {
            return Err(refusal);
        }
        let usable = match self.usable() {
            Some(usable) => usable,
            None => self.connect_leg().await?,
        };
        match usable {
            Usable::H2(sender) => Ok(Route::H2(sender)),
            Usable::Pool(pool) => pool.checkout(&self.cx, POOL_WAIT).await.map(Route::H1),
        }
    }

    fn usable(&self) -> Option<Usable> {
        match &*self.leg.lock().unwrap_or_else(PoisonError::into_inner) {
            LegState::H2(conn) if !conn.sender.is_closed() => Some(Usable::H2(conn.sender.clone())),
            LegState::H1(pool) => Some(Usable::Pool(pool.clone())),
            LegState::None | LegState::H2(_) => None,
        }
    }

    /// Connects to the real server, once for every stream that is waiting for it (each waits at most
    /// the upstream-head time for the attempt in progress): a stream that
    /// came while an attempt was running gets that attempt's failure, not an attempt of its own,
    /// so a dead server costs one timeout however many streams ask.
    async fn connect_leg(&self) -> Result<Usable, Refusal> {
        let asked = Instant::now();
        let mut gate = tokio::time::timeout(
            self.cx.proxy.config.upstream_head_timeout,
            self.connecting.lock(),
        )
        .await
        .map_err(|_| {
            Refusal::new(
                "503 Service Unavailable",
                format!(
                    "the connection to {} is still being set up",
                    self.cx.target.host
                ),
            )
        })?;
        if let Some(usable) = self.usable() {
            return Ok(usable);
        }
        if let Some((failed_at, refusal)) = gate.as_ref()
            && *failed_at >= asked
        {
            return Err(refusal.clone());
        }
        let made = match leg::connect(&self.cx, &[ALPN_H2, ALPN_HTTP11]).await {
            Ok(made) => made,
            Err(refusal) => {
                *gate = Some((Instant::now(), refusal.clone()));
                return Err(refusal);
            }
        };
        {
            let mut outcome = self.outcome.lock().unwrap_or_else(PoisonError::into_inner);
            if outcome.hop.is_none() && outcome.resolved_ip.is_none() {
                outcome.hop = made.hop;
                outcome.resolved_ip = made.ip;
            }
        }
        let mut leg = self.leg.lock().unwrap_or_else(PoisonError::into_inner);
        Ok(match made.conn {
            Connected::H2(conn) => {
                let sender = conn.sender.clone();
                *leg = LegState::H2(conn);
                Usable::H2(sender)
            }
            Connected::H1(conn) => {
                let pool = H1Pool::new(conn, Arc::clone(&self.cx.connections));
                *leg = LegState::H1(pool.clone());
                Usable::Pool(pool)
            }
        })
    }

    /// The place in the workspace's stream budget for one more stream, as a guard that keeps it.
    fn open_stream(&self) -> Result<StreamGuard, Refusal> {
        let Ok(permit) = Arc::clone(&self.cx.streams).try_acquire_owned() else {
            tracing::warn!(workspace = %self.cx.workspace, host = %self.cx.target.host, "HTTP/2 stream refused: the workspace is over its stream budget");
            return Err(Refusal::new(
                "503 Service Unavailable",
                "too many open requests from this workspace; retry when some have finished",
            )
            .header("retry-after", "1")
            .coded(ReasonCode::StreamLimit));
        };
        Ok(self
            .streams
            .open(self.cx.proxy.config.stream_idle_timeout, Some(permit)))
    }
}

/// Serves the guest's HTTP/2 connection `io` until it ends; `first` is the verified connection
/// made at the handshake (if it could be made, else `deferred` says why).
pub(crate) async fn serve<IO>(
    cx: Arc<Context>,
    io: IO,
    first: Option<Connected>,
    deferred: Option<Refusal>,
    outcome: Outcome,
) -> Outcome
where
    IO: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let config = cx.proxy.config;
    let leg = match first {
        Some(Connected::H2(conn)) => LegState::H2(conn),
        Some(Connected::H1(conn)) => LegState::H1(H1Pool::new(conn, Arc::clone(&cx.connections))),
        None => LegState::None,
    };
    let limit_hit: Arc<OnceLock<LimitCode>> = Arc::default();
    let unknown_tokens = Arc::new(AnswerReport::of(&cx));
    let shared = Arc::new(Shared {
        cx,
        leg: Mutex::new(leg),
        connecting: tokio::sync::Mutex::new(None),
        deferred: Mutex::new(deferred),
        outcome: Mutex::new(outcome),
        scrubbed: Arc::default(),
        unknown_tokens,
        streams: Arc::new(Registry::new(Arc::clone(&limit_hit))),
    });
    let service = {
        let shared = Arc::clone(&shared);
        service_fn(move |request| handle(Arc::clone(&shared), request))
    };
    let mut builder = http2::Builder::new(TokioExecutor::new());
    builder
        .timer(TokioTimer::new())
        .initial_stream_window_size(GUEST_STREAM_WINDOW)
        .initial_connection_window_size(GUEST_CONNECTION_WINDOW)
        .max_frame_size(leg::MAX_FRAME_SIZE)
        .max_concurrent_streams(MAX_CONCURRENT_STREAMS)
        .max_header_list_size(MAX_REQUEST_HEADER_LIST)
        .max_pending_accept_reset_streams(MAX_PENDING_ACCEPT_RESETS)
        .max_local_error_reset_streams(MAX_LOCAL_ERROR_RESETS)
        .keep_alive_interval(leg::PING_INTERVAL)
        .keep_alive_timeout(leg::PING_TIMEOUT)
        .enable_connect_protocol();
    let watched = Watch::new(io, config.head_timeout, Arc::clone(&limit_hit));
    let connection = builder.serve_connection(TokioIo::new(watched), service);
    tokio::pin!(connection);
    let mut sweeper = Sweeper::new(
        Clock {
            keepalive: config.keepalive_timeout,
            grace: config.shutdown_grace,
        },
        Instant::now(),
    );
    let mut tick = tokio::time::interval(SWEEP_INTERVAL);
    loop {
        tokio::select! {
            result = connection.as_mut() => {
                if let Err(err) = result {
                    tracing::debug!(host = %shared.cx.target.host, error = %err, "guest HTTP/2 connection ended");
                }
                break;
            }
            _ = tick.tick() => match sweeper.tick(Instant::now(), &shared.streams) {
                Step::Nothing => {}
                Step::Shutdown => connection.as_mut().graceful_shutdown(),
                Step::Drop(code) => {
                    tracing::info!(host = %shared.cx.target.host, reason = %code, "guest HTTP/2 connection dropped");
                    // the first limit hit wins; a later one finds the cell already set
                    let _ = limit_hit.set(code);
                    break;
                }
            }
        }
    }
    let mut outcome = shared
        .outcome
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    outcome.ended = limit_hit.get().copied();
    outcome.record_scrubbed(&shared.scrubbed);
    outcome.record_unknown_tokens(&shared.unknown_tokens);
    std::mem::take(&mut *outcome)
}

async fn handle(
    shared: Arc<Shared>,
    request: Request<Incoming>,
) -> Result<Response<RespBody>, Infallible> {
    let answered = match shared.open_stream() {
        Ok(guard) => {
            let activity = guard.activity();
            tokio::select! {
                answered = exchange(&shared, request, guard) => answered,
                () = activity.expired() => {
                    activity.note_limit(ended::STREAM_IDLE);
                    Err(stream_idle())
                }
            }
        }
        Err(refusal) => Err(refusal),
    };
    Ok(match answered {
        Ok(response) => response,
        Err(refusal) => {
            if let Some(code) = refusal.code {
                shared
                    .outcome
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .refused
                    .get_or_insert(code);
            }
            refusal_response(&refusal)
        }
    })
}

/// The answer to a stream that moved no frame for its limit before the proxy had one to give.
fn stream_idle() -> Refusal {
    tracing::info!("an HTTP/2 stream moved no frame for too long and was reset");
    Refusal::new(
        "408 Request Timeout",
        "the request moved no data for too long",
    )
}

/// A refusal as an HTTP/2 response: the same status, headers and text as the HTTP/1.1 path sends.
fn refusal_response(refusal: &Refusal) -> Response<RespBody> {
    let status = refusal
        .status
        .split(' ')
        .next()
        .and_then(|code| code.parse::<u16>().ok())
        .and_then(|code| StatusCode::from_u16(code).ok())
        .unwrap_or(StatusCode::BAD_GATEWAY);
    let text = format!("puddle: {}\n", refusal.message);
    let mut response = Response::new(
        Full::new(Bytes::from(text))
            .map_err(|never| match never {})
            .boxed_unsync(),
    );
    *response.status_mut() = status;
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    for (name, value) in &refusal.headers {
        if let (Ok(name), Ok(value)) = (
            HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_str(value),
        ) {
            headers.insert(name, value);
        }
    }
    response
}

/// What the checks learn about a request.
#[derive(Debug)]
pub(crate) struct Checked {
    pub(crate) method: Method,
    /// Path and query, starting with `/`.
    pub(crate) path: String,
    /// The `:protocol` of an extended `CONNECT` (RFC 8441), if any.
    pub(crate) protocol: Option<hyper::ext::Protocol>,
}

/// Checks an HTTP/2 request the way the strict HTTP/1.1 parser checks one: the request must be
/// for the connection's own host, in origin form, with nothing that only makes sense on an
/// HTTP/1.1 connection.
pub(crate) fn check<B>(
    request: &Request<B>,
    target: &crate::request_target::Target,
) -> Result<Checked, Refusal> {
    let protocol = request.extensions().get::<hyper::ext::Protocol>().cloned();
    let method = request.method().clone();
    if method == Method::CONNECT && protocol.is_none() {
        return Err(bad("CONNECT inside a tunnel is not supported"));
    }
    if protocol.is_some() && method != Method::CONNECT {
        return Err(bad("a :protocol on a request that is not CONNECT"));
    }
    let headers = request.headers();
    let uri = request.uri();
    let mut hosts = headers.get_all(header::HOST).iter();
    let host = hosts.next();
    if hosts.next().is_some() {
        return Err(bad("more than one Host header"));
    }
    let host = host
        .map(|value| value.to_str().map_err(|_| bad("malformed Host header")))
        .transpose()?;
    let authority = uri.authority().map(::http::uri::Authority::as_str);
    if authority.is_none() && host.is_none() {
        return Err(bad("missing :authority"));
    }
    for named in authority.into_iter().chain(host) {
        if named.contains('@') {
            return Err(bad("userinfo (user@host) is not allowed"));
        }
        if !request::authority_is(named, target) {
            return Err(misdirected(target));
        }
    }
    match uri.scheme_str() {
        None | Some("https") => {}
        Some(_) => return Err(bad("an absolute http:// target inside an https connection")),
    }
    let path = uri
        .path_and_query()
        .map(|path| path.as_str().to_owned())
        .unwrap_or_default();
    if !path.starts_with('/') {
        return Err(bad("the request target must be a path"));
    }
    // Connection-specific fields have no meaning on HTTP/2 and are how a request is smuggled
    // past a proxy that translates it to HTTP/1.1 (RFC 9113 §8.2.2).
    for name in [
        header::CONNECTION,
        HeaderName::from_static("keep-alive"),
        HeaderName::from_static("proxy-connection"),
        header::TRANSFER_ENCODING,
        header::UPGRADE,
    ] {
        if headers.contains_key(&name) {
            return Err(bad(format!("the {name} header does not exist in HTTP/2")));
        }
    }
    for value in headers.get_all(header::TE) {
        if !value.as_bytes().eq_ignore_ascii_case(b"trailers") {
            return Err(bad("TE other than trailers does not exist in HTTP/2"));
        }
    }
    let mut lengths = headers.get_all(header::CONTENT_LENGTH).iter().map(|value| {
        value
            .to_str()
            .ok()
            .and_then(|text| text.parse::<u64>().ok())
    });
    if let Some(first) = lengths.next() {
        let Some(first) = first else {
            return Err(bad("invalid Content-Length"));
        };
        if lengths.any(|length| length != Some(first)) {
            return Err(bad("conflicting Content-Length headers"));
        }
    }
    // `Expect: 100-continue` is accepted but never answered with a `100` on HTTP/2: hyper's
    // HTTP/2 server cannot send an interim response. The client sends its body when its own wait
    // runs out (curl: 1 s) and the request goes on as usual; HTTP/1.1 does get the `100`.
    for value in headers.get_all(header::EXPECT) {
        if !value.as_bytes().eq_ignore_ascii_case(b"100-continue") {
            return Err(Refusal::new(
                "417 Expectation Failed",
                "only Expect: 100-continue is supported",
            )
            .coded(ReasonCode::ExpectationUnsupported));
        }
    }
    Ok(Checked {
        method,
        path,
        protocol,
    })
}

/// The request's header lines for the injector: the same `name: value` lines the HTTP/1.1 head
/// has, with the `Host` the HTTP/2 request names in `:authority`.
fn header_lines(headers: &HeaderMap, host: &str) -> Vec<String> {
    let mut lines = Vec::with_capacity(headers.len() + 1);
    lines.push(format!("host: {host}"));
    for (name, value) in headers {
        if name == header::HOST {
            continue;
        }
        lines.push(format!(
            "{}: {}",
            name.as_str(),
            String::from_utf8_lossy(value.as_bytes())
        ));
    }
    lines
}

async fn exchange(
    shared: &Arc<Shared>,
    mut request: Request<Incoming>,
    guard: StreamGuard,
) -> Result<Response<RespBody>, Refusal> {
    let cx = &shared.cx;
    let checked = check(&request, &cx.target).inspect_err(|refusal| {
        tracing::info!(host = %cx.target.host, status = %refusal.status, "terminated request refused: {}", refusal.message);
    })?;
    let guest_upgrade = checked
        .protocol
        .is_some()
        .then(|| hyper::upgrade::on(&mut request));
    let (parts, mut body) = request.into_parts();
    let lines = header_lines(&parts.headers, &cx.target.host.to_string());
    let req = GuestRequest {
        method: checked.method.as_str(),
        target: &checked.path,
        lines: &lines,
        headers: GuestHeaders::Map(&parts.headers),
        extended_connect: checked.protocol.is_some(),
    };
    let mut audit = RequestAudit::default();
    let prepared = prepare(shared, &checked, &req, &mut body, &mut audit).await;
    shared
        .outcome
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .absorb(cx, audit);
    let (mut route, prepared) = prepared?;
    let Prepared {
        headers,
        target,
        body: prefetched,
        mut exchange,
        injection,
        unauthorized,
    } = prepared;
    let path = target
        .as_ref()
        .map_or(checked.path.as_str(), |path| path.as_str());
    if let Some(guest_upgrade) = guest_upgrade {
        return websocket(shared, guest_upgrade, &checked, path, route, headers, guard).await;
    }
    let version = route.version();
    let head_only = checked.method == Method::HEAD;
    let activity = guard.activity();
    let sent = Arc::new(Sent::default());
    let body = request_body(body, prefetched, injection.as_ref(), &activity, &sent);
    let upstream_request = build_request(cx, version, checked.method, path, headers, body)?;
    let response = send(shared, &mut route, upstream_request, &sent).await?;
    if let Some(refusal) = pipeline::replaced_401(cx, response.status(), unauthorized.as_ref()) {
        return Err(refusal);
    }
    let response = pipeline::answer(
        cx,
        exchange.as_mut(),
        response,
        head_only,
        &shared.scrubbed,
        &shared.unknown_tokens,
    )
    .await?;
    let lease = match route {
        Route::H1(lease) => Some(lease),
        Route::H2(_) => None,
    };
    Ok(guest_response(response, head_only, &sent, lease, guard))
}

/// The request side of the pipeline for one stream, with the route to the server taken between
/// the rules and the injector. A route taken for a request that is then refused goes back unused.
async fn prepare(
    shared: &Shared,
    checked: &Checked,
    req: &GuestRequest<'_>,
    body: &mut Incoming,
    audit: &mut RequestAudit,
) -> Result<(Route, Prepared), Refusal> {
    let cx = &shared.cx;
    pipeline::admit(cx, req, audit)?;
    if let Some(protocol) = &checked.protocol
        && protocol.as_str() != "websocket"
    {
        return Err(Refusal::new(
            "501 Not Implemented",
            format!("the :protocol {} is not supported", protocol.as_str()),
        )
        .coded(ReasonCode::ProtocolUnsupported));
    }
    let route = shared.route().await?;
    let mut wire = H2Wire { cx, body };
    match pipeline::prepare(cx, &mut wire, req, route.version(), audit).await {
        Ok(prepared) => Ok((route, prepared)),
        Err(refusal) => {
            route.unused();
            Err(refusal)
        }
    }
}

/// The body of the request sent upstream: the one that was read in full already, or the guest's
/// own, streamed.
fn request_body(
    body: Incoming,
    prefetched: Option<Bytes>,
    injection: Option<&Injection>,
    activity: &Arc<Activity>,
    sent: &Arc<Sent>,
) -> UpBody {
    if let Some(whole) = prefetched {
        sent.finish();
        return Full::new(whole)
            .map_err(|never| match never {})
            .boxed_unsync();
    }
    let injected = injection
        .map(|injection| {
            injection
                .headers()
                .iter()
                .map(|header| header.name().clone())
                .collect()
        })
        .unwrap_or_default();
    GuestBody::new(body, activity, sent, injected).boxed_unsync()
}

/// The request for the real server: `:authority` and `:scheme` from the URI on HTTP/2, the
/// origin-form target (the `Host` header is in `headers`) on HTTP/1.1.
fn build_request(
    cx: &Context,
    version: UpstreamVersion,
    method: Method,
    path: &str,
    headers: HeaderMap,
    body: UpBody,
) -> Result<Request<UpBody>, Refusal> {
    let uri = match version {
        UpstreamVersion::H2 => Uri::builder()
            .scheme("https")
            .authority(cx.target.host.to_string())
            .path_and_query(path)
            .build(),
        UpstreamVersion::H1 => path.parse::<Uri>().map_err(Into::into),
    }
    .map_err(|_: ::http::Error| bad("malformed request target"))?;
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .version(match version {
            UpstreamVersion::H2 => ::http::Version::HTTP_2,
            UpstreamVersion::H1 => ::http::Version::HTTP_11,
        });
    if let Some(map) = builder.headers_mut() {
        *map = headers;
    }
    builder.body(body).map_err(|_| bad("malformed request"))
}

/// Sends `request` over `route` and waits for the answer's head: `502` when the request fails,
/// `504` when the server stays silent for the upstream-head timeout after the request is fully
/// sent (`sent`), or does not take the request up in that time (a stream queued behind the
/// server's own stream limit). A streaming call that never finishes sending is held by the idle
/// timeout.
async fn send(
    shared: &Shared,
    route: &mut Route,
    request: Request<UpBody>,
    sent: &Sent,
) -> Result<Response<Incoming>, Refusal> {
    let cx = &shared.cx;
    let timeout = cx.proxy.config.upstream_head_timeout;
    let head_deadline = async {
        if tokio::time::timeout(timeout, sent.started()).await.is_err() {
            return Late::NotTaken;
        }
        sent.wait().await;
        tokio::time::sleep(timeout).await;
        Late::Silent
    };
    let sending = async {
        match route {
            Route::H2(sender) => Some(send_h2(shared, sender, request).await),
            Route::H1(lease) => match lease.sender() {
                Some(sender) => Some(sender.send_request(request).await),
                None => None,
            },
        }
    };
    tokio::select! {
        answered = sending => match answered {
            Some(Ok(response)) => Ok(response),
            Some(Err(err)) => {
                tracing::info!(host = %cx.target.host, error = %err, "upstream request failed");
                Err(Refusal::new(
                    "502 Bad Gateway",
                    format!("the request to {} failed: {}", cx.target.host, short(&err)),
                ))
            }
            None => Err(Refusal::new("502 Bad Gateway", "the upstream connection is gone")),
        },
        late = head_deadline => Err(Refusal::new(
            "504 Gateway Timeout",
            match late {
                Late::NotTaken => format!(
                    "{} did not take up the request within {}s",
                    cx.target.host,
                    timeout.as_secs()
                ),
                Late::Silent => format!(
                    "{} did not answer within {}s",
                    cx.target.host,
                    timeout.as_secs()
                ),
            },
        )),
    }
}

/// Sends `request` on the upstream HTTP/2 connection. A request the connection gives back unsent
/// (the server said GOAWAY, which the connection only acts on when the next request comes) goes
/// once more on a new connection, as a client talking to the server directly sends it again: the
/// server never saw it.
async fn send_h2(
    shared: &Shared,
    sender: &mut hyper::client::conn::http2::SendRequest<UpBody>,
    request: Request<UpBody>,
) -> Result<Response<Incoming>, hyper::Error> {
    let mut failed = match sender.try_send_request(request).await {
        Ok(response) => return Ok(response),
        Err(failed) => failed,
    };
    let Some(request) = failed.take_message() else {
        return Err(failed.into_error());
    };
    tracing::debug!(host = %shared.cx.target.host, error = %failed.error(), "upstream connection closed before it took the request; connecting again");
    match shared.connect_leg().await {
        Ok(Usable::H2(fresh)) => {
            *sender = fresh;
            sender.send_request(request).await
        }
        // The server now speaks HTTP/1.1 (the request was built for HTTP/2), or cannot be reached.
        Ok(Usable::Pool(_)) | Err(_) => Err(failed.into_error()),
    }
}

/// Which wait of [`send`] ran out.
enum Late {
    /// The request never started: the connection's stream limit held it back.
    NotTaken,
    /// The request was sent in full and the server never answered.
    Silent,
}

/// An extended `CONNECT` for a WebSocket (RFC 8441): to an HTTP/2 server it is passed on as it is;
/// to an HTTP/1.1 server it becomes the `Upgrade` handshake of RFC 6455 with a key of the
/// proxy's own. Either way the answer is a `200` and the two connections become a byte pipe.
async fn websocket(
    shared: &Arc<Shared>,
    guest_upgrade: hyper::upgrade::OnUpgrade,
    checked: &Checked,
    path: &str,
    mut route: Route,
    mut headers: HeaderMap,
    guard: StreamGuard,
) -> Result<Response<RespBody>, Refusal> {
    let cx = &shared.cx;
    let version = route.version();
    let mut key = None;
    let method = match version {
        UpstreamVersion::H2 => Method::CONNECT,
        UpstreamVersion::H1 => {
            if !ws::version_ok(&headers) {
                return Err(bad("only Sec-WebSocket-Version 13 is supported"));
            }
            let fresh = ws::new_key().ok_or_else(no_random_bytes)?;
            headers.insert(header::CONNECTION, HeaderValue::from_static("upgrade"));
            headers.insert(header::UPGRADE, HeaderValue::from_static("websocket"));
            headers.insert(
                HeaderName::from_static("sec-websocket-key"),
                HeaderValue::from_str(&fresh).map_err(|_| bad("key"))?,
            );
            key = Some(fresh);
            Method::GET
        }
    };
    let empty: UpBody = http_body_util::Empty::<Bytes>::new()
        .map_err(|never| match never {})
        .boxed_unsync();
    let mut upstream_request = build_request(cx, version, method, path, headers, empty)?;
    if version == UpstreamVersion::H2
        && let Some(protocol) = &checked.protocol
    {
        upstream_request.extensions_mut().insert(protocol.clone());
    }
    let finished = Sent::default();
    finished.finish();
    let mut response = send(shared, &mut route, upstream_request, &finished).await?;
    let agreed = match version {
        UpstreamVersion::H2 => response.status().is_success(),
        UpstreamVersion::H1 => response.status() == StatusCode::SWITCHING_PROTOCOLS,
    };
    let lease = match route {
        Route::H1(lease) => Some(lease),
        Route::H2(_) => None,
    };
    if !agreed {
        // The server said no: its answer goes to the guest as any other.
        let response = pipeline::answer(
            cx,
            None,
            response,
            false,
            &shared.scrubbed,
            &shared.unknown_tokens,
        )
        .await?;
        let sent = Arc::new(Sent::default());
        sent.finish();
        return Ok(guest_response(response, false, &sent, lease, guard));
    }
    if let Some(key) = &key
        && !ws::accept_matches(response.headers(), key)
    {
        return Err(Refusal::new(
            "502 Bad Gateway",
            "the server's WebSocket answer was not valid",
        ));
    }
    let upstream_upgrade = hyper::upgrade::on(&mut response);
    let mut answer = Response::new(
        http_body_util::Empty::<Bytes>::new()
            .map_err(|never| match never {})
            .boxed_unsync(),
    );
    cx.termination.stand_ins().scrub_answer_headers(
        response.headers_mut(),
        &cx.target.host,
        &shared.scrubbed,
    );
    for (name, value) in ws::answer_headers(response.headers(), false) {
        answer.headers_mut().append(name, value);
    }
    let activity = guard.activity();
    let idle = cx.proxy.config.websocket_idle_timeout;
    activity.set_limit(idle);
    tokio::spawn(async move {
        let _guard = guard;
        let _lease = lease;
        // a failed upgrade is logged inside; the task ends whether or not the splice ran
        let _ = async {
            let guest = guest_upgrade
                .await
                .inspect_err(|err| tracing::debug!(error = %err, "the guest's WebSocket upgrade did not complete"))
                .ok()?;
            let upstream = upstream_upgrade
                .await
                .inspect_err(|err| tracing::debug!(error = %err, "the server's WebSocket upgrade did not complete"))
                .ok()?;
            ws::splice(&mut TokioIo::new(guest), upstream, &activity, idle).await;
            Some(())
        }
        .await;
    });
    Ok(answer)
}

/// The system had no random bytes for a WebSocket key.
fn no_random_bytes() -> Refusal {
    Refusal::new("502 Bad Gateway", "no random bytes for the WebSocket key")
}

/// Headers of a response that describe the upstream connection, not the message.
fn is_connection_specific(name: &HeaderName) -> bool {
    matches!(
        name.as_str(),
        "connection"
            | "keep-alive"
            | "proxy-connection"
            | "proxy-authenticate"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
    )
}

/// `headers` without those that describe one connection (`Connection` and what it names,
/// `Keep-Alive`, `Transfer-Encoding`, `Upgrade`, ...): they mean nothing on the guest's HTTP/2
/// connection.
fn end_to_end_headers(headers: &HeaderMap) -> HeaderMap {
    let named_in_connection: Vec<String> = headers
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(|token| token.trim().to_ascii_lowercase())
        .collect();
    let mut kept = HeaderMap::with_capacity(headers.len());
    for (name, value) in headers {
        if is_connection_specific(name) || named_in_connection.iter().any(|t| t == name.as_str()) {
            continue;
        }
        kept.append(name.clone(), value.clone());
    }
    kept
}

/// The upstream's response, for the HTTP/2 guest: connection-specific headers removed, the body
/// (and its trailers) streamed through.
fn guest_response(
    response: Response<AnswerBody>,
    head_only: bool,
    sent: &Arc<Sent>,
    lease: Option<Lease>,
    guard: StreamGuard,
) -> Response<RespBody> {
    let (mut parts, body) = response.into_parts();
    parts.headers = end_to_end_headers(&parts.headers);
    super::alt_svc::strip_h3(&mut parts.headers);
    parts.version = ::http::Version::HTTP_2;
    let body: RespBody = if head_only {
        // `HEAD` and `304` describe a body they do not carry; an HTTP/2 stream just ends.
        release(lease, sent);
        drop(guard);
        http_body_util::Empty::<Bytes>::new()
            .map_err(|never| match never {})
            .boxed_unsync()
    } else {
        UpstreamBody::new(body, sent, lease, guard).boxed_unsync()
    };
    Response::from_parts(parts, body)
}

/// The guest's HTTP/2 stream, as the pipeline reads a request body from it.
struct H2Wire<'a> {
    cx: &'a Context,
    body: &'a mut Incoming,
}

impl Wire for H2Wire<'_> {
    type Stop = Refusal;

    /// Reads the body's data frames; trailers are not read into it, and the request goes on
    /// without them.
    async fn read_body(&mut self, limit: usize) -> Result<Bytes, Refusal> {
        let too_large = || body_too_large(limit);
        let declared = self.body.size_hint().exact();
        if declared.is_some_and(|n| n > limit as u64) {
            return Err(too_large());
        }
        let idle = self.cx.proxy.config.body_idle_timeout;
        let mut whole = Vec::new();
        loop {
            match tokio::time::timeout(idle, self.body.frame()).await {
                Err(_) => {
                    return Err(Refusal::new(
                        "408 Request Timeout",
                        "the request body did not arrive",
                    )
                    .coded(ReasonCode::RequestTimeout));
                }
                Ok(None) => break,
                Ok(Some(Err(err))) => {
                    tracing::info!(host = %self.cx.target.host, error = %err, "request body failed");
                    return Err(bad("the request body could not be read"));
                }
                Ok(Some(Ok(frame))) => {
                    if let Some(data) = frame.data_ref() {
                        whole.extend_from_slice(data);
                        if whole.len() > limit {
                            return Err(too_large());
                        }
                    }
                }
            }
        }
        // A guest may end a stream cleanly short of its `Content-Length` (as `GuestBody` guards).
        if declared.is_some_and(|n| n != whole.len() as u64) {
            return Err(bad("the request body ended before its Content-Length"));
        }
        Ok(Bytes::from(whole))
    }
}

// ---------------------------------------------------------------------------------------------
// Bodies

/// Whether the request body has been taken up and read to its end, and the waits for that.
#[derive(Debug, Default)]
pub(crate) struct Sent {
    begun: AtomicBool,
    done: AtomicBool,
    notify: Notify,
}

impl Sent {
    /// The upstream took the request up: its stream is open and it reads the body.
    fn begin(&self) {
        self.begun.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    fn finish(&self) {
        self.done.store(true, Ordering::SeqCst);
        self.begin();
    }

    async fn started(&self) {
        loop {
            let notified = self.notify.notified();
            if self.begun.load(Ordering::SeqCst) {
                return;
            }
            notified.await;
        }
    }

    fn is_done(&self) -> bool {
        self.done.load(Ordering::SeqCst)
    }

    async fn wait(&self) {
        loop {
            let notified = self.notify.notified();
            if self.is_done() {
                return;
            }
            notified.await;
        }
    }
}

fn timed_out(activity: &Activity) -> BoxError {
    activity.note_limit(ended::STREAM_IDLE);
    tracing::info!("an HTTP/2 stream moved no frame for too long and was reset");
    Box::new(io::Error::new(
        io::ErrorKind::TimedOut,
        "the stream was idle for too long",
    ))
}

/// The guest's request body on its way upstream.
///
/// A guest may end a stream with `RST_STREAM(NO_ERROR)` after fewer bytes than its
/// `Content-Length`; the HTTP/2 library reports that as a clean end. Forwarded as it is, the
/// upstream request would be framed as complete with a body shorter than it declared, which is how
/// a server that translates HTTP/2 to HTTP/1.1 behind us gets desynchronised. So the bytes are
/// counted and a body that ends short is an error, which resets the upstream stream.
struct GuestBody<B> {
    inner: B,
    activity: Arc<Activity>,
    sent: Arc<Sent>,
    /// The `Content-Length` the guest declared, if any.
    expected: Option<u64>,
    received: u64,
    /// Headers the proxy sets itself: a guest may not send them in a trailer either.
    injected: Vec<HeaderName>,
}

impl<B: Body<Data = Bytes>> GuestBody<B> {
    fn new(
        inner: B,
        activity: &Arc<Activity>,
        sent: &Arc<Sent>,
        injected: Vec<HeaderName>,
    ) -> Self {
        if inner.is_end_stream() {
            sent.finish();
        }
        activity.touch();
        Self {
            expected: inner.size_hint().exact(),
            received: 0,
            injected,
            inner,
            activity: Arc::clone(activity),
            sent: Arc::clone(sent),
        }
    }

    /// Whether the bytes seen are the `Content-Length` the guest declared (or none was declared).
    fn complete(&self) -> bool {
        self.expected
            .is_none_or(|expected| expected == self.received)
    }
}

/// Takes out of a trailer block what must not travel there (RFC 9110 §6.5.1: framing, routing and
/// authentication fields) and what the proxy injected into the head.
fn scrub_trailers(trailers: &mut HeaderMap, injected: &[HeaderName]) {
    let names: Vec<HeaderName> = trailers
        .keys()
        .filter(|name| {
            request::is_proxy_owned(name)
                || matches!(
                    name.as_str(),
                    "authorization"
                        | "cookie"
                        | "content-encoding"
                        | "content-type"
                        | "content-range"
                )
                || injected.contains(name)
        })
        .cloned()
        .collect();
    for name in names {
        trailers.remove(&name);
    }
}

fn body_ended_short() -> BoxError {
    Box::new(io::Error::new(
        io::ErrorKind::UnexpectedEof,
        "the request body ended before its Content-Length",
    ))
}

impl<B> Body for GuestBody<B>
where
    B: Body<Data = Bytes> + Unpin,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    type Data = Bytes;
    type Error = BoxError;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, BoxError>>> {
        let this = &mut *self;
        // The upstream polls the body once it has a stream for it.
        this.sent.begin();
        match Pin::new(&mut this.inner).poll_frame(cx) {
            Poll::Ready(Some(Ok(mut frame))) => {
                this.activity.touch();
                if let Some(data) = frame.data_ref() {
                    this.received = this
                        .received
                        .saturating_add(u64::try_from(data.len()).unwrap_or(u64::MAX));
                }
                if let Some(trailers) = frame.trailers_mut() {
                    scrub_trailers(trailers, &this.injected);
                }
                if this.inner.is_end_stream() && this.complete() {
                    this.sent.finish();
                }
                Poll::Ready(Some(Ok(frame)))
            }
            Poll::Ready(Some(Err(err))) => Poll::Ready(Some(Err(Box::new(err)))),
            Poll::Ready(None) => {
                if !this.complete() {
                    return Poll::Ready(Some(Err(body_ended_short())));
                }
                this.sent.finish();
                Poll::Ready(None)
            }
            Poll::Pending => {
                if this.activity.poll_expired(cx).is_ready() {
                    Poll::Ready(Some(Err(timed_out(&this.activity))))
                } else {
                    Poll::Pending
                }
            }
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

/// The upstream's response body on its way to the guest. It holds the HTTP/1.1 connection it
/// came over, which goes back to the pool once the body ended cleanly and the request was fully
/// sent.
struct UpstreamBody<B> {
    inner: B,
    activity: Arc<Activity>,
    sent: Arc<Sent>,
    lease: Option<Lease>,
    _guard: StreamGuard,
}

impl<B: Body<Data = Bytes>> UpstreamBody<B> {
    fn new(inner: B, sent: &Arc<Sent>, lease: Option<Lease>, guard: StreamGuard) -> Self {
        let activity = guard.activity();
        activity.touch();
        let mut body = Self {
            inner,
            activity,
            sent: Arc::clone(sent),
            lease,
            _guard: guard,
        };
        if body.inner.is_end_stream() {
            body.finished();
        }
        body
    }

    /// The response ended: the connection is reusable only if the request ended too.
    fn finished(&mut self) {
        release(self.lease.take(), &self.sent);
    }
}

/// Gives an HTTP/1.1 connection back to its pool when the request it carried was sent in full.
fn release(lease: Option<Lease>, sent: &Sent) {
    if let Some(mut lease) = lease
        && sent.is_done()
    {
        lease.mark_reusable();
    }
}

impl<B> Body for UpstreamBody<B>
where
    B: Body<Data = Bytes> + Unpin,
    B::Error: Into<BoxError>,
{
    type Data = Bytes;
    type Error = BoxError;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, BoxError>>> {
        let this = &mut *self;
        match Pin::new(&mut this.inner).poll_frame(cx) {
            Poll::Ready(Some(Ok(frame))) => {
                this.activity.touch();
                if this.inner.is_end_stream() {
                    this.finished();
                }
                Poll::Ready(Some(Ok(frame)))
            }
            Poll::Ready(Some(Err(err))) => {
                this.lease = None;
                Poll::Ready(Some(Err(err.into())))
            }
            Poll::Ready(None) => {
                this.finished();
                Poll::Ready(None)
            }
            Poll::Pending => {
                if this.activity.poll_expired(cx).is_ready() {
                    this.lease = None;
                    Poll::Ready(Some(Err(timed_out(&this.activity))))
                } else {
                    Poll::Pending
                }
            }
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

#[cfg(test)]
mod tests;
