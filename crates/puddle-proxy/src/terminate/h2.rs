// SPDX-License-Identifier: GPL-3.0-or-later
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
use puddle_types::HttpRequestLine;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::Notify;
use tokio::time::Instant;

use super::exchange::{AnswerBody, Exchange, MAX_EXCHANGE_BODY, read_answer, swap_request_body};
use super::guest::{ALPN_H2, ALPN_HTTP11};
use super::inject::{
    Forwarding, InjectContext, Injection, RequestView, Unauthorized, body_too_large,
};
use super::leg::{self, BoxError, Connected, H1Pool, H2Conn, Lease, UpBody};
use super::request::{self, UpstreamVersion, bad, misdirected};
use super::request_rules::{self, Refused};
use super::session::{Context, Outcome};
use super::streams::{
    Activity, Clock, Registry, SWEEP_INTERVAL, Step, StreamGuard, Sweeper, ended,
};
use super::watch::Watch;
use super::ws;
use crate::proxy::Refusal;

/// Streams a guest may have open at once (RFC 9113 §6.5.2). Real tools open a few dozen; the
/// bar is the hundreds `PX-40` names for connections.
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
        if let Some(refusal) = self
            .deferred
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
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
            _ => None,
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
            .coded("stream_limit"));
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
    let limit_hit: Arc<OnceLock<&'static str>> = Arc::default();
    let shared = Arc::new(Shared {
        cx,
        leg: Mutex::new(leg),
        connecting: tokio::sync::Mutex::new(None),
        deferred: Mutex::new(deferred),
        outcome: Mutex::new(outcome),
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
                    tracing::info!(host = %shared.cx.target.host, reason = code, "guest HTTP/2 connection dropped");
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
    target: &crate::target::Target,
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
    for value in headers.get_all(header::EXPECT) {
        if !value.as_bytes().eq_ignore_ascii_case(b"100-continue") {
            return Err(Refusal::new(
                "417 Expectation Failed",
                "only Expect: 100-continue is supported",
            )
            .coded("expectation_unsupported"));
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
    {
        let mut outcome = shared
            .outcome
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if outcome.http.is_none() {
            outcome.http = Some(HttpRequestLine::new(checked.method.as_str(), &checked.path));
        }
    }
    tracing::debug!(host = %cx.target.host, method = %checked.method, path = %puddle_types::request_path(&checked.path), "terminated request (h2)");
    refuse_by_rule(shared, &checked, request.headers())?;
    if let Some(protocol) = &checked.protocol
        && protocol.as_str() != "websocket"
    {
        return Err(Refusal::new(
            "501 Not Implemented",
            format!("the :protocol {} is not supported", protocol.as_str()),
        )
        .coded("protocol_unsupported"));
    }
    let mut route = shared.route().await?;
    let version = route.version();
    let prefetched = match body_wanted(shared, &checked, request.headers(), version) {
        Some(limit) => match read_whole_body(shared, request.body_mut(), limit).await {
            Ok(body) => Some(body),
            Err(refusal) => {
                route.unused();
                return Err(refusal);
            }
        },
        None => None,
    };
    let Forwarding {
        injection,
        unauthorized,
    } = match decide(
        shared,
        &checked,
        request.headers(),
        version,
        prefetched.as_deref(),
    )
    .await
    {
        Ok(forwarding) => forwarding,
        Err(refusal) => {
            route.unused();
            return Err(refusal);
        }
    };
    let mut headers =
        request::upstream_headers_h2(request.headers(), &cx.target, injection.as_ref(), version)?;
    record_injection(shared, injection.as_ref());
    swap_stand_ins(shared, &mut headers, &checked.path, injection.as_ref());
    if checked.protocol.is_some() {
        let guest_upgrade = hyper::upgrade::on(&mut request);
        return websocket(shared, guest_upgrade, &checked, route, headers, guard).await;
    }
    let (mut exchange, prefetched) =
        match begin_exchange(shared, &checked, &mut request, &mut headers, prefetched).await {
            Ok(begun) => begun,
            Err(refusal) => {
                route.unused();
                return Err(refusal);
            }
        };
    let head_only = checked.method == Method::HEAD;
    let activity = guard.activity();
    let sent = Arc::new(Sent::default());
    let (_, body) = request.into_parts();
    let body = request_body(body, prefetched, injection.as_ref(), &activity, &sent);
    let upstream_request =
        build_request(cx, version, checked.method, &checked.path, headers, body)?;
    let response = send(shared, &mut route, upstream_request, &sent).await?;
    if let Some(refusal) = unauthorized_answer(shared, &response, unauthorized.as_ref()) {
        return Err(refusal);
    }
    let response = read_exchange_answer(shared, exchange.as_mut(), response).await?;
    let lease = match route {
        Route::H1(lease) => Some(lease),
        Route::H2(_) => None,
    };
    Ok(guest_response(response, head_only, &sent, lease, guard))
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

/// Asks the workspace's exchange rewriter about the request and, if it is a token exchange, gets
/// it ready as the HTTP/1.1 path does: the answer will be read, so the upstream is asked for it
/// unencoded; and the body is read in full and has its stand-ins swapped for the real values when
/// the exchange asks for that.
async fn begin_exchange(
    shared: &Shared,
    checked: &Checked,
    request: &mut Request<Incoming>,
    headers: &mut HeaderMap,
    prefetched: Option<Bytes>,
) -> Result<(Option<Exchange>, Option<Bytes>), Refusal> {
    let cx = &shared.cx;
    let began = cx.termination.exchanges().and_then(|rewriter| {
        rewriter.begin(
            &cx.target.host,
            checked.method.as_str(),
            puddle_types::request_path(&checked.path),
        )
    });
    let Some(exchange) = began else {
        return Ok((None, prefetched));
    };
    let mut exchange = exchange;
    if exchange.answer_mut().is_some() {
        headers.remove(header::ACCEPT_ENCODING);
    }
    if !exchange.wants_request_body() {
        return Ok((Some(exchange), prefetched));
    }
    let body = match prefetched {
        Some(body) => body,
        None => read_whole_body(shared, request.body_mut(), MAX_EXCHANGE_BODY).await?,
    };
    let content_type = request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok());
    let host = &cx.target.host;
    let (swapped, report) = swap_request_body(
        cx.termination.stand_ins(),
        &exchange,
        content_type,
        &body,
        host,
    );
    shared
        .outcome
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .record_stand_ins(host, &report);
    Ok((Some(exchange), Some(swapped.unwrap_or(body))))
}

/// Lets the exchange's rewriter read the answer, when it has one to read.
async fn read_exchange_answer(
    shared: &Shared,
    exchange: Option<&mut Exchange>,
    response: Response<Incoming>,
) -> Result<Response<AnswerBody>, Refusal> {
    let Some(answer) = exchange.and_then(Exchange::answer_mut) else {
        return Ok(response.map(AnswerBody::Streaming));
    };
    let cx = &shared.cx;
    read_answer(answer, response, cx.proxy.config.body_idle_timeout)
        .await
        .map_err(|err| {
            tracing::info!(host = %cx.target.host, error = %err, "the answer could not be read");
            Refusal::new(
                "502 Bad Gateway",
                format!("{} did not finish its answer", cx.target.host),
            )
        })
}

/// Refuses the request when a rule on method and path names it, before anything is sent
/// upstream, as the HTTP/1.1 path does. Only this stream is refused; the connection goes on.
fn refuse_by_rule(shared: &Shared, checked: &Checked, headers: &HeaderMap) -> Result<(), Refusal> {
    let cx = &shared.cx;
    let lines = header_lines(headers, &cx.target.host.to_string());
    let view = RequestView::new(checked.method.as_str(), &checked.path, &lines);
    let Some(rule) = cx
        .termination
        .request_rules()
        .refusing(&cx.target.host, &view)
    else {
        return Ok(());
    };
    tracing::info!(host = %cx.target.host, %rule, "request refused by a rule on method and path (h2)");
    shared
        .outcome
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .refused_by_rule
        .get_or_insert_with(|| Refused {
            rule,
            line: HttpRequestLine::new(checked.method.as_str(), &checked.path),
        });
    Err(request_rules::refusal(
        rule,
        checked.method.as_str(),
        &cx.target.host,
    ))
}

/// Asks the injector about the request, as the HTTP/1.1 path does: what it may add, or why the
/// request is refused.
async fn decide(
    shared: &Shared,
    checked: &Checked,
    headers: &HeaderMap,
    version: UpstreamVersion,
    body: Option<&[u8]>,
) -> Result<Forwarding, Refusal> {
    let cx = &shared.cx;
    let (method, lines) = injector_lines(checked, headers, version, &cx.target.host.to_string());
    let context = InjectContext {
        workspace: &cx.workspace,
        host: &cx.target.host,
    };
    let view = RequestView::new(method, &checked.path, &lines);
    let decision = match body {
        Some(body) => {
            cx.termination
                .injector()
                .decide(&context, &view.with_body(body))
                .await
        }
        None => cx.termination.injector().decide(&context, &view).await,
    };
    decision.into_forwarding().map_err(|refusal| {
        tracing::info!(host = %cx.target.host, code = refusal.code(), "request refused by the credential rules");
        refusal.to_refusal()
    })
}

/// The method and header lines the injector sees. A WebSocket over an extended CONNECT reaches an
/// HTTP/1.1 server as a `GET` with `Upgrade` headers; the rules judge what the server will be sent.
fn injector_lines<'a>(
    checked: &'a Checked,
    headers: &HeaderMap,
    version: UpstreamVersion,
    host: &str,
) -> (&'a str, Vec<String>) {
    let mut lines = header_lines(headers, host);
    if checked.protocol.is_some() && version == UpstreamVersion::H1 {
        lines.push("connection: upgrade".to_owned());
        lines.push("upgrade: websocket".to_owned());
        return (Method::GET.as_str(), lines);
    }
    (checked.method.as_str(), lines)
}

/// How much of the request body the injector wants to see before it decides, if any.
fn body_wanted(
    shared: &Shared,
    checked: &Checked,
    headers: &HeaderMap,
    version: UpstreamVersion,
) -> Option<usize> {
    let cx = &shared.cx;
    if checked.protocol.is_some() {
        return None;
    }
    let (method, lines) = injector_lines(checked, headers, version, &cx.target.host.to_string());
    let context = InjectContext {
        workspace: &cx.workspace,
        host: &cx.target.host,
    };
    let view = RequestView::new(method, &checked.path, &lines);
    cx.termination.injector().body_wanted(&context, &view)
}

/// Reads the guest's whole request body (at most `limit` bytes) for an injector that decides on
/// it. Trailers are not read into it, and the request goes on without them.
async fn read_whole_body(
    shared: &Shared,
    body: &mut Incoming,
    limit: usize,
) -> Result<Bytes, Refusal> {
    let too_large = || body_too_large(limit);
    let declared = body.size_hint().exact();
    if declared.is_some_and(|n| n > limit as u64) {
        return Err(too_large());
    }
    let idle = shared.cx.proxy.config.body_idle_timeout;
    let mut whole = Vec::new();
    loop {
        match tokio::time::timeout(idle, body.frame()).await {
            Err(_) => {
                return Err(
                    Refusal::new("408 Request Timeout", "the request body did not arrive")
                        .coded("request_timeout"),
                );
            }
            Ok(None) => break,
            Ok(Some(Err(err))) => {
                tracing::info!(host = %shared.cx.target.host, error = %err, "request body failed");
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

/// The refusal that replaces the server's answer when it is a `401` for a request whose
/// credential was Puddle's to choose.
fn unauthorized_answer<B>(
    shared: &Shared,
    response: &Response<B>,
    unauthorized: Option<&Unauthorized>,
) -> Option<Refusal> {
    if response.status() != StatusCode::UNAUTHORIZED {
        return None;
    }
    let refusal = unauthorized?.refusal()?;
    tracing::info!(host = %shared.cx.target.host, code = refusal.code(), "401 replaced: the credential for this request was Puddle's to choose");
    Some(refusal.to_refusal())
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
            Route::H2(sender) => Some(sender.send_request(request).await),
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
    let mut upstream_request = build_request(cx, version, method, &checked.path, headers, empty)?;
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
        let sent = Arc::new(Sent::default());
        sent.finish();
        return Ok(guest_response(
            response.map(AnswerBody::Streaming),
            false,
            &sent,
            lease,
            guard,
        ));
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
    for (name, value) in ws::answer_headers(response.headers(), false) {
        answer.headers_mut().append(name, value);
    }
    let activity = guard.activity();
    let idle = cx.proxy.config.websocket_idle_timeout;
    activity.set_limit(idle);
    tokio::spawn(async move {
        let _guard = guard;
        let _lease = lease;
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

/// Swaps the workspace's stand-ins in `headers` for their real values, as the HTTP/1.1 path does
/// (the injected headers are left alone), reports the ones in the request `target`, and notes
/// both for the audit.
fn swap_stand_ins(
    shared: &Shared,
    headers: &mut HeaderMap,
    target: &str,
    injection: Option<&Injection>,
) {
    let host = &shared.cx.target.host;
    let swapped = shared
        .cx
        .termination
        .stand_ins()
        .swap(headers, host, injection);
    let in_target = shared.cx.termination.stand_ins().scan_target(target);
    let mut outcome = shared
        .outcome
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    outcome.record_stand_ins(host, &swapped);
    outcome.record_stand_ins(host, &in_target);
}

fn record_injection(shared: &Shared, injection: Option<&Injection>) {
    if let Some(injection) = injection {
        let mut outcome = shared
            .outcome
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        outcome.injected = true;
        outcome
            .binding_id
            .get_or_insert_with(|| injection.binding_id().to_owned());
    }
}

fn short(err: &hyper::Error) -> String {
    use std::error::Error as _;
    err.source()
        .map_or_else(|| err.to_string(), ToString::to_string)
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
    B::Error: std::error::Error + Send + Sync + 'static,
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
                Poll::Ready(Some(Err(Box::new(err))))
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
mod tests {
    use puddle_types::Host;

    use super::*;
    use crate::target::Target;

    fn target() -> Target {
        Target {
            host: Host::parse_normalised("bound.test").unwrap(),
            port: 443,
        }
    }

    fn builder() -> ::http::request::Builder {
        Request::builder().uri("https://bound.test/a?b=1")
    }

    fn status(refusal: &Refusal) -> &str {
        refusal.status.split(' ').next().unwrap()
    }

    #[test]
    fn a_request_for_the_connection_host_passes_with_its_path_and_query() {
        let request = builder().body(()).unwrap();
        let checked = check(&request, &target()).unwrap();
        assert_eq!(checked.method, Method::GET);
        assert_eq!(checked.path, "/a?b=1");
        assert!(checked.protocol.is_none());
        // `Host` and `:authority` may both be there if they agree, with or without the port.
        for host in ["bound.test", "Bound.Test:443"] {
            let request = builder().header("host", host).body(()).unwrap();
            assert!(check(&request, &target()).is_ok(), "{host}");
        }
    }

    #[test]
    fn a_request_for_another_host_or_in_another_form_is_refused() {
        for (what, request, code) in [
            (
                "another authority",
                Request::builder()
                    .uri("https://other.test/")
                    .body(())
                    .unwrap(),
                "421",
            ),
            (
                "another port",
                Request::builder()
                    .uri("https://bound.test:8443/")
                    .body(())
                    .unwrap(),
                "421",
            ),
            (
                "a Host that disagrees",
                builder().header("host", "other.test").body(()).unwrap(),
                "421",
            ),
            (
                "two Host headers",
                builder()
                    .header("host", "bound.test")
                    .header("host", "bound.test")
                    .body(())
                    .unwrap(),
                "400",
            ),
            (
                "a Host that is not text",
                builder()
                    .header("host", HeaderValue::from_bytes(b"bound\xff.test").unwrap())
                    .body(())
                    .unwrap(),
                "400",
            ),
            (
                "no authority at all",
                Request::builder().uri("/a").body(()).unwrap(),
                "400",
            ),
            (
                "userinfo",
                Request::builder()
                    .uri("https://user@bound.test/")
                    .body(())
                    .unwrap(),
                "400",
            ),
            (
                "http scheme",
                Request::builder()
                    .uri("http://bound.test/")
                    .body(())
                    .unwrap(),
                "400",
            ),
        ] {
            let refusal = check(&request, &target()).unwrap_err();
            assert_eq!(status(&refusal), code, "{what}: {}", refusal.message);
        }
    }

    #[test]
    fn headers_that_do_not_exist_in_http2_are_refused() {
        for (name, value) in [
            ("connection", "keep-alive"),
            ("keep-alive", "timeout=5"),
            ("proxy-connection", "keep-alive"),
            ("transfer-encoding", "chunked"),
            ("upgrade", "h2c"),
            ("te", "gzip"),
        ] {
            let request = builder().header(name, value).body(()).unwrap();
            let refusal = check(&request, &target()).unwrap_err();
            assert_eq!(status(&refusal), "400", "{name}");
        }
        let request = builder().header("te", "Trailers").body(()).unwrap();
        assert!(check(&request, &target()).is_ok());
    }

    #[test]
    fn content_length_and_expect_are_checked() {
        for (values, ok) in [
            (vec!["10"], true),
            (vec!["10", "10"], true),
            (vec!["10", "11"], false),
            (vec!["ten"], false),
            (vec!["-1"], false),
        ] {
            let mut request = builder();
            for value in &values {
                request = request.header("content-length", *value);
            }
            let result = check(&request.body(()).unwrap(), &target());
            assert_eq!(result.is_ok(), ok, "{values:?}");
        }
        let request = builder().header("expect", "100-continue").body(()).unwrap();
        assert!(check(&request, &target()).is_ok());
        let request = builder().header("expect", "gimme").body(()).unwrap();
        assert_eq!(status(&check(&request, &target()).unwrap_err()), "417");
    }

    #[test]
    fn connect_is_for_websockets_only() {
        let plain = Request::builder()
            .method("CONNECT")
            .uri("https://bound.test/")
            .body(())
            .unwrap();
        assert_eq!(status(&check(&plain, &target()).unwrap_err()), "400");
        let websocket = Request::builder()
            .method("CONNECT")
            .uri("https://bound.test/chat")
            .extension(hyper::ext::Protocol::from_static("websocket"))
            .body(())
            .unwrap();
        let checked = check(&websocket, &target()).unwrap();
        assert_eq!(
            checked.protocol.as_ref().map(hyper::ext::Protocol::as_str),
            Some("websocket")
        );
        let stray = builder()
            .extension(hyper::ext::Protocol::from_static("websocket"))
            .body(())
            .unwrap();
        assert_eq!(status(&check(&stray, &target()).unwrap_err()), "400");
    }

    #[test]
    fn the_injector_sees_the_lines_of_an_http11_head_with_the_host() {
        let mut headers = HeaderMap::new();
        headers.insert("host", HeaderValue::from_static("bound.test"));
        headers.append("cookie", HeaderValue::from_static("a=1"));
        headers.append("cookie", HeaderValue::from_static("b=2"));
        headers.insert("x-bytes", HeaderValue::from_bytes(b"a\xffb").unwrap());
        let lines = header_lines(&headers, "bound.test");
        assert_eq!(lines.first().map(String::as_str), Some("host: bound.test"));
        assert_eq!(lines.iter().filter(|l| l.starts_with("host:")).count(), 1);
        assert_eq!(lines.iter().filter(|l| l.starts_with("cookie:")).count(), 2);
        assert!(lines.iter().any(|l| l.starts_with("x-bytes: a")));
    }

    #[test]
    fn a_refusal_becomes_a_response_with_its_status_headers_and_text() {
        let refusal = Refusal::new("403 Forbidden", "nope").header("x-puddle-blocked", "why");
        let response = refusal_response(&refusal);
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(response.headers()["x-puddle-blocked"], "why");
        assert_eq!(
            response.headers()["content-type"],
            "text/plain; charset=utf-8"
        );
        let odd = refusal_response(&Refusal::new("garbage", "x"));
        assert_eq!(odd.status(), StatusCode::BAD_GATEWAY);
    }

    #[test]
    fn trailers_lose_framing_routing_authentication_and_injected_names() {
        let mut trailers = HeaderMap::new();
        for name in [
            "x-kept",
            "grpc-status",
            "authorization",
            "cookie",
            "host",
            "content-length",
            "transfer-encoding",
            "x-api-key",
        ] {
            trailers.insert(
                HeaderName::from_bytes(name.as_bytes()).unwrap(),
                "v".parse().unwrap(),
            );
        }
        scrub_trailers(&mut trailers, &[HeaderName::from_static("x-api-key")]);
        let mut left: Vec<&str> = trailers.keys().map(HeaderName::as_str).collect();
        left.sort_unstable();
        assert_eq!(left, ["grpc-status", "x-kept"]);
    }

    #[test]
    fn connection_specific_response_headers_are_dropped() {
        for name in [
            "connection",
            "keep-alive",
            "transfer-encoding",
            "upgrade",
            "te",
            "trailer",
        ] {
            assert!(
                is_connection_specific(&HeaderName::from_static(name)),
                "{name}"
            );
        }
        assert!(!is_connection_specific(&HeaderName::from_static(
            "content-type"
        )));
    }

    #[test]
    fn a_response_loses_the_headers_of_its_http11_connection_and_those_it_names() {
        let mut headers = HeaderMap::new();
        headers.append("connection", "keep-alive, X-Hop".parse().unwrap());
        headers.append("keep-alive", "timeout=5".parse().unwrap());
        headers.append("x-hop", "only for this connection".parse().unwrap());
        headers.append("x-kept", "one".parse().unwrap());
        headers.append("x-kept", "two".parse().unwrap());
        headers.append("content-type", "text/plain".parse().unwrap());
        let kept = end_to_end_headers(&headers);
        let mut names: Vec<&str> = kept.keys().map(HeaderName::as_str).collect();
        names.sort_unstable();
        assert_eq!(names, ["content-type", "x-kept"]);
        assert_eq!(kept.get_all("x-kept").iter().count(), 2);
    }

    #[test]
    fn the_refusal_without_random_bytes_is_a_bad_gateway() {
        assert_eq!(no_random_bytes().status, "502 Bad Gateway");
    }

    #[tokio::test]
    async fn the_sent_flag_wakes_its_waiters_once_and_stays_set() {
        let sent = Arc::new(Sent::default());
        let waiter = {
            let sent = Arc::clone(&sent);
            tokio::spawn(async move { sent.wait().await })
        };
        tokio::task::yield_now().await;
        assert!(!sent.is_done());
        sent.finish();
        waiter.await.unwrap();
        sent.wait().await;
    }

    /// A body that yields the frames it is given, then ends or never answers.
    struct Scripted {
        frames: std::collections::VecDeque<Result<Frame<Bytes>, io::Error>>,
        exact: Option<u64>,
        then_pending: bool,
    }

    impl Scripted {
        fn new(frames: Vec<Result<Frame<Bytes>, io::Error>>, exact: Option<u64>) -> Self {
            Self {
                frames: frames.into(),
                exact,
                then_pending: false,
            }
        }

        fn then_pending(mut self) -> Self {
            self.then_pending = true;
            self
        }
    }

    impl Body for Scripted {
        type Data = Bytes;
        type Error = io::Error;

        fn poll_frame(
            mut self: Pin<&mut Self>,
            _cx: &mut TaskContext<'_>,
        ) -> Poll<Option<Result<Frame<Bytes>, io::Error>>> {
            match self.frames.pop_front() {
                Some(frame) => Poll::Ready(Some(frame)),
                None if self.then_pending => Poll::Pending,
                None => Poll::Ready(None),
            }
        }

        fn is_end_stream(&self) -> bool {
            self.frames.is_empty() && !self.then_pending
        }

        fn size_hint(&self) -> SizeHint {
            self.exact
                .map_or_else(SizeHint::default, SizeHint::with_exact)
        }
    }

    fn data(text: &'static str) -> Frame<Bytes> {
        Frame::data(Bytes::from_static(text.as_bytes()))
    }

    const IDLE: Duration = Duration::from_secs(3600);

    fn guest_body(inner: Scripted) -> (GuestBody<Scripted>, Arc<Sent>) {
        let (body, sent, _) = guest_body_in(inner, &Arc::new(Registry::default()));
        (body, sent)
    }

    fn guest_body_in(
        inner: Scripted,
        registry: &Arc<Registry>,
    ) -> (GuestBody<Scripted>, Arc<Sent>, StreamGuard) {
        let sent = Arc::new(Sent::default());
        let guard = registry.open(IDLE, None);
        let body = GuestBody::new(
            inner,
            &guard.activity(),
            &sent,
            vec![HeaderName::from_static("x-api-key")],
        );
        (body, sent, guard)
    }

    fn upstream_body(inner: Scripted, sent: &Arc<Sent>) -> (UpstreamBody<Scripted>, Arc<Registry>) {
        let registry = Arc::new(Registry::default());
        let body = UpstreamBody::new(inner, sent, None, registry.open(IDLE, None));
        (body, registry)
    }

    #[tokio::test]
    async fn a_request_body_that_ends_short_of_its_content_length_is_an_error() {
        let (mut body, sent) = guest_body(Scripted::new(vec![Ok(data("12345"))], Some(10)));
        assert!(body.frame().await.unwrap().unwrap().is_data());
        let err = body.frame().await.unwrap().unwrap_err();
        assert!(
            err.to_string().contains("before its Content-Length"),
            "{err}"
        );
        assert!(
            !sent.is_done(),
            "a truncated request is never marked as sent"
        );
    }

    #[tokio::test]
    async fn a_request_body_with_its_whole_length_ends_and_is_marked_sent() {
        let (mut body, sent) = guest_body(Scripted::new(
            vec![Ok(data("12")), Ok(data("345"))],
            Some(5),
        ));
        assert!(body.frame().await.unwrap().unwrap().is_data());
        assert!(body.frame().await.unwrap().unwrap().is_data());
        assert!(body.frame().await.is_none());
        assert!(sent.is_done());
        assert_eq!(body.size_hint().exact(), Some(5));
    }

    #[tokio::test]
    async fn a_request_body_without_a_declared_length_ends_at_its_end() {
        let (mut body, sent) = guest_body(Scripted::new(vec![Ok(data("12"))], None));
        assert!(body.frame().await.unwrap().unwrap().is_data());
        assert!(body.frame().await.is_none());
        assert!(sent.is_done());
    }

    #[tokio::test]
    async fn an_error_in_the_request_body_is_passed_on() {
        let (mut body, _) = guest_body(Scripted::new(
            vec![Err(io::Error::other("the guest's stream broke"))],
            None,
        ));
        let err = body.frame().await.unwrap().unwrap_err();
        assert!(err.to_string().contains("stream broke"), "{err}");
    }

    /// Runs the connection's sweep every tick for `span`, as the connection loop does.
    async fn sweep_for(registry: &Registry, span: Duration) {
        let mut waited = Duration::ZERO;
        while waited < span {
            tokio::time::advance(SWEEP_INTERVAL).await;
            waited += SWEEP_INTERVAL;
            registry.sweep_for_test(Instant::now());
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_request_body_that_moves_nothing_for_an_hour_is_reset_by_the_sweep() {
        let registry = Arc::new(Registry::default());
        let (mut body, _, _guard) =
            guest_body_in(Scripted::new(vec![], None).then_pending(), &registry);
        let frame = tokio::spawn(async move { body.frame().await });
        tokio::task::yield_now().await;
        assert!(!frame.is_finished());
        sweep_for(&registry, IDLE + Duration::from_secs(4)).await;
        let err = frame.await.unwrap().unwrap().unwrap_err();
        let err = err.downcast_ref::<io::Error>().unwrap();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
    }

    #[tokio::test(start_paused = true)]
    async fn a_request_body_that_is_polled_is_taken_up_and_one_that_is_not_is_waited_for() {
        let (mut body, sent) = guest_body(Scripted::new(vec![Ok(data("a"))], None).then_pending());
        let waiting = tokio::time::timeout(Duration::from_secs(5), sent.started()).await;
        assert!(waiting.is_err(), "nothing polled the body yet");
        assert!(body.frame().await.is_some());
        sent.started().await;
        assert!(!sent.is_done(), "taken up is not sent in full");
    }

    #[tokio::test]
    async fn an_empty_request_body_is_sent_at_once() {
        let (body, sent) = guest_body(Scripted::new(vec![], Some(0)));
        assert!(body.is_end_stream());
        assert!(sent.is_done());
    }

    #[tokio::test]
    async fn a_response_body_passes_its_frames_and_counts_the_stream_until_it_is_dropped() {
        let sent = Arc::new(Sent::default());
        sent.finish();
        let (mut body, open) = upstream_body(Scripted::new(vec![Ok(data("ab"))], Some(2)), &sent);
        assert_eq!(open.len(), 1);
        assert_eq!(body.size_hint().exact(), Some(2));
        assert!(body.frame().await.unwrap().unwrap().is_data());
        assert!(body.frame().await.is_none());
        assert!(body.is_end_stream());
        drop(body);
        assert_eq!(open.len(), 0);
    }

    #[tokio::test]
    async fn an_error_in_the_response_body_is_passed_on() {
        let sent = Arc::new(Sent::default());
        let (mut body, _) = upstream_body(
            Scripted::new(vec![Err(io::Error::other("the server broke"))], None),
            &sent,
        );
        let err = body.frame().await.unwrap().unwrap_err();
        assert!(err.to_string().contains("server broke"), "{err}");
    }

    #[tokio::test(start_paused = true)]
    async fn a_response_body_that_moves_nothing_for_an_hour_is_reset_by_the_sweep() {
        let sent = Arc::new(Sent::default());
        let (mut body, registry) = upstream_body(Scripted::new(vec![], None).then_pending(), &sent);
        let frame = tokio::spawn(async move { body.frame().await });
        tokio::task::yield_now().await;
        sweep_for(&registry, IDLE + Duration::from_secs(4)).await;
        let err = frame.await.unwrap().unwrap().unwrap_err();
        let err = err.downcast_ref::<io::Error>().unwrap();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
    }

    #[test]
    fn a_target_that_is_not_a_path_is_refused() {
        let request = Request::builder()
            .method(Method::OPTIONS)
            .uri("*")
            .header("host", "bound.test")
            .body(())
            .unwrap();
        let refusal = check(&request, &target()).unwrap_err();
        assert_eq!(status(&refusal), "400");
    }
}
