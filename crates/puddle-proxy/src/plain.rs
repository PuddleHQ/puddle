// SPDX-License-Identifier: AGPL-3.0-or-later
//! Plain HTTP: absolute-form `http://` requests, any number of them on one guest connection.
//!
//! Each request is handled as if it had come on a connection of its own: decided by the rules for
//! its own host, judged by the rules on method and path, written as its own audit record, and
//! sent upstream rebuilt from its parsed parts. What the connection adds is reuse: a request for
//! the host and port of the one before it goes out on the same upstream connection, as long as
//! the address that connection reached still passes the checks the request just went through.
//!
//! Requests are read one at a time. The next head is read only once the previous response is
//! written, so a pipelined request waits in the buffer and is decided on its own. A connection
//! whose request body was not read to its end (the server answered early, the body failed) is
//! closed after the response instead of read on, so the rest of a body is never taken for a
//! request. A refusal ends the connection too, for the same reason: its body was never read.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use ::http::HeaderValue;
use ::http::header::{HOST, PROXY_AUTHORIZATION};
use http_body_util::BodyExt as _;
use hyper_util::rt::TokioIo;
use puddle_types::{
    ConnectionDecision, ConnectionEvent, ConnectionOutcome, ConnectionReason, EgressRequest,
    HttpRequestLine, ProtocolHint, ReasonCode,
};
use puddle_upstream::RouteWatch;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

use crate::counted::ByteCounts;
use crate::http::{Body, Head};
use crate::proxy::{
    ClientReader, Proxy, ProxyConfig, Refusal, WorkspaceHandler, admit, parse_request,
    read_request, refuse, refuse_by_request_rule,
};
use crate::request_target::Target;
use crate::terminate::h1::{
    self, Abort, AnswerBody, BoxError, ChannelBody, H1Conn, Parsed, UpBody, Written,
};
use crate::upstream::{Admitted, Out, ProxyForm, connect_out};

/// What feeds a request body to the upstream client.
type BodySender = tokio::sync::mpsc::Sender<bytes::Bytes>;

/// One plain-HTTP request whose head passed the checks that need no rule.
pub(crate) struct Incoming {
    head: Head,
    target: Target,
    parsed: Parsed,
}

impl Incoming {
    /// Checks a request [`parse_request`] read as plain HTTP (`path` is its origin-form target).
    ///
    /// # Errors
    /// The refusal for a request no rule needs to see: framing, `Expect` or version trouble.
    pub(crate) fn new(head: Head, target: Target, path: String) -> Result<Self, Refusal> {
        let mut parsed = h1::framed(&head, path)?;
        // `Upgrade` is a hop-by-hop header; nothing plain HTTP carries is switched to another
        // protocol on the way through.
        parsed.upgrade = false;
        Ok(Self {
            head,
            target,
            parsed,
        })
    }

    /// As [`Self::new`], and a refusal is also written to the audit as a blocked record with its
    /// code, as on a decrypted connection: the target was parsed, so the request is one to audit.
    pub(crate) async fn checked(
        handler: &WorkspaceHandler,
        head: Head,
        target: Target,
        path: String,
    ) -> Result<Self, Refusal> {
        let line = HttpRequestLine::new(head.method.as_str(), &path);
        let seen = target.clone();
        match Self::new(head, target, path) {
            Ok(incoming) => Ok(incoming),
            Err(refusal) => {
                // Every refusal of a parsed head carries a code; one that did not is a bad request.
                let code = refusal.code.unwrap_or(ReasonCode::BadRequest);
                let request = EgressRequest::new(handler.workspace.clone(), seen.host, seen.port)
                    .with_protocol(ProtocolHint::Http);
                let mut event = ConnectionEvent::new(
                    &request,
                    ConnectionDecision::Blocked,
                    ConnectionReason::Refused(code),
                );
                event.http = Some(line);
                handler.proxy.record(event).await;
                Err(refusal)
            }
        }
    }

    fn has_body(&self) -> bool {
        !matches!(self.parsed.body, Body::None | Body::Length(0))
    }
}

/// How the guest connection goes on after a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flow {
    /// Read the next request.
    Continue,
    /// The response is complete; end the connection cleanly.
    Close,
    /// Nothing more is sent: a refusal already ended the stream, or the response broke off.
    Stop,
}

/// The upstream connection a guest connection keeps between requests.
struct Kept {
    target: Target,
    conn: H1Conn,
    /// The address connected to (directly, or the one a proxy was given), if known.
    addr: Option<SocketAddr>,
    hop: Option<String>,
    via: Option<ProxyForm>,
    watch: Option<RouteWatch>,
}

impl Kept {
    /// Starts HTTP/1.1 on `out`, a connection made for `target`.
    async fn begin(out: Out, target: &Target, ended: &Arc<AtomicU64>) -> Result<Self, Refusal> {
        let name = target.host.to_string();
        let conn = h1::handshake(TokioIo::new(out.stream), ended)
            .await
            .map_err(|err| h1::setup_refusal(&name, &h1::short(&err)))?;
        Ok(Self {
            target: target.clone(),
            conn,
            addr: out.addr,
            hop: out.hop,
            via: out.via,
            watch: out.watch,
        })
    }

    /// Whether a request for `target`, admitted as `admitted`, may go out on this connection: the
    /// same host and port, an address the request's own checks let through, no network change
    /// since it was made, and a connection the server left open after its last answer.
    ///
    /// Any network change counts, even one whose route still has this connection's hop: asking
    /// the new route may wait (a PAC script), and a new connection costs one connect.
    async fn fits(&mut self, target: &Target, admitted: &Admitted) -> bool {
        let address_passes = match self.addr {
            Some(addr) => admitted.addrs.contains(&addr),
            None => admitted.name_ok,
        };
        let network_moved = self.watch.as_ref().is_some_and(RouteWatch::epoch_moved);
        if self.target != *target || !address_passes || network_moved {
            return false;
        }
        // `ready` settles once the last response was read to its end; a connection the server
        // closed, or fed bytes it never asked for, is not ready.
        matches!(
            tokio::time::timeout(h1::READY_WAIT, self.conn.sender.ready()).await,
            Ok(Ok(()))
        )
    }
}

/// Serves plain-HTTP requests on one guest connection, starting with `first`, until the guest
/// closes, goes quiet, asks to close, reaches the request limit, or a request ends it. `counts`
/// are the connection's byte counts; each request's record gets the bytes moved while it ran.
pub(crate) async fn serve<S>(
    handler: &WorkspaceHandler,
    reader: ClientReader<S>,
    counts: &ByteCounts,
    first: Incoming,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    let proxy = &handler.proxy;
    let config = proxy.config;
    let (read, mut writer) = tokio::io::split(reader);
    let mut reader = BufReader::new(read);
    let mut kept: Option<Kept> = None;
    let mut next = Some(first);
    let mut served: usize = 0;
    let mut mark = (0, 0);
    while let Some(incoming) = next.take() {
        served += 1;
        let last = served >= config.max_requests_per_connection;
        let mut guest = Guest {
            reader: &mut reader,
            writer: &mut writer,
        };
        let (flow, mut event) = one_request(handler, &mut guest, &mut kept, &incoming, last).await;
        let now = (counts.read(), counts.written());
        event.bytes_up = now.0 - mark.0;
        event.bytes_down = now.1 - mark.1;
        mark = now;
        match flow {
            Flow::Continue => {
                // The record waits on the audit's thread; the next request need not wait on it.
                let (read_next, ()) = tokio::join!(
                    next_request(handler, &mut reader, &mut writer, &config),
                    proxy.record(event),
                );
                next = read_next;
            }
            Flow::Close => {
                proxy.record(event).await;
                if let Err(err) = writer.shutdown().await {
                    tracing::debug!(error = %err, "guest went away before the close");
                }
            }
            Flow::Stop => proxy.record(event).await,
        }
    }
}

/// The guest's two halves, read and written apart so the read side can be polled while a response
/// is written (see [`h1::keep_reading`]).
struct Guest<'a, R, W> {
    reader: &'a mut R,
    writer: &'a mut W,
}

/// Waits for the guest's next request (at most the keep-alive time for its first byte, the head
/// time for the rest of its head) and checks it. `None`: the connection ends, cleanly when the
/// guest closed or went quiet, with a refusal when the head was refused.
async fn next_request<R, W>(
    handler: &WorkspaceHandler,
    reader: &mut R,
    writer: &mut W,
    config: &ProxyConfig,
) -> Option<Incoming>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let waited = tokio::time::timeout(config.keepalive_timeout, reader.fill_buf()).await;
    if !matches!(waited, Ok(Ok(buf)) if !buf.is_empty()) {
        if let Err(err) = writer.shutdown().await {
            tracing::debug!(error = %err, "guest went away before the close");
        }
        return None;
    }
    let refusal = match read_request(reader, config.head_timeout).await {
        Ok(head) => match parse_request(&head) {
            Ok((target, Some(path), _)) => {
                match Incoming::checked(handler, head, target, path).await {
                    Ok(incoming) => return Some(incoming),
                    Err(refusal) => refusal,
                }
            }
            Ok((_, None, _)) => Refusal::new(
                "400 Bad Request",
                "CONNECT on a connection that carried plain HTTP; send it on a connection of its own",
            ),
            Err(refusal) => refusal,
        },
        Err(None) => return None,
        Err(Some(refusal)) => refusal,
    };
    refuse(writer, &refusal).await;
    None
}

/// Decides, connects (or reuses the kept connection) and forwards one request; returns how the
/// connection goes on and the request's audit record (bytes are filled in by the caller).
async fn one_request<R, W>(
    handler: &WorkspaceHandler,
    guest: &mut Guest<'_, R, W>,
    kept: &mut Option<Kept>,
    incoming: &Incoming,
    last: bool,
) -> (Flow, ConnectionEvent)
where
    R: AsyncBufRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    let proxy = &handler.proxy;
    let target = &incoming.target;
    let request = EgressRequest::new(handler.workspace.clone(), target.host.clone(), target.port)
        .with_protocol(ProtocolHint::Http);
    let (admitted, mut event) = match proxy
        .admit_distribution_point(
            &request,
            incoming.head.method.as_str(),
            puddle_types::request_path(&incoming.parsed.target),
        )
        .await
    {
        Some((admitted, event)) => (Ok(admitted), event),
        None => admit(proxy, &request).await,
    };
    event.http = Some(HttpRequestLine::new(
        incoming.head.method.as_str(),
        &incoming.parsed.target,
    ));
    let admitted = match admitted {
        Ok(admitted) => admitted,
        Err(refusal) => {
            event.outcome = refusal.outcome;
            refuse(guest.writer, &refusal).await;
            return (Flow::Stop, event);
        }
    };
    if let Some((refusal, change)) = refuse_by_request_rule(
        proxy,
        &request,
        &incoming.head,
        Some(&incoming.parsed.target),
    ) {
        change(&mut event);
        refuse(guest.writer, &refusal).await;
        return (Flow::Stop, event);
    }
    // A connection that does not fit is closed before a new one is made: a guest connection holds
    // one upstream socket at a time. It goes back only when the exchange left it usable.
    let mut reused = kept.take();
    if let Some(conn) = reused.as_mut()
        && !conn.fits(target, &admitted).await
    {
        reused = None;
    }
    let mut conn = match reused {
        Some(conn) => conn,
        None => match connect(proxy, target, &admitted).await {
            Ok(conn) => conn,
            Err(refusal) => {
                event.outcome = refusal.outcome;
                refuse(guest.writer, &refusal).await;
                return (Flow::Stop, event);
            }
        },
    };
    event.outcome = Some(ConnectionOutcome::Connected);
    event.resolved_ip = conn.addr.map(|addr| addr.ip());
    event.upstream.clone_from(&conn.hop);
    let flow = exchange(&proxy.config, guest, &mut conn, incoming, last).await;
    if flow == Flow::Continue {
        *kept = Some(conn);
    }
    (flow, event)
}

/// Connects out for a plain-HTTP request to `target` and starts HTTP/1.1 on the connection.
async fn connect(proxy: &Proxy, target: &Target, admitted: &Admitted) -> Result<Kept, Refusal> {
    let out = connect_out(
        proxy.upstream(),
        target,
        false,
        admitted,
        proxy.config.connect_timeout,
    )
    .await?;
    Kept::begin(out, target, &proxy.upstream_ended).await
}

/// Serves the one plain-HTTP request of a connection that carries only it (the image-pull
/// listener's way) on `out`, an upstream connection the caller made and checked: the same
/// exchange a workspace's plain HTTP gets, with the answer saying it closes. Then the guest side
/// is closed; the upstream connection is dropped.
pub(crate) async fn forward_once<S>(
    config: &ProxyConfig,
    ended: &Arc<AtomicU64>,
    reader: ClientReader<S>,
    out: Out,
    incoming: &Incoming,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    let (read, mut writer) = tokio::io::split(reader);
    let mut reader = BufReader::new(read);
    let mut conn = match Kept::begin(out, &incoming.target, ended).await {
        Ok(conn) => conn,
        Err(refusal) => {
            refuse(&mut writer, &refusal).await;
            return;
        }
    };
    let mut guest = Guest {
        reader: &mut reader,
        writer: &mut writer,
    };
    if exchange(config, &mut guest, &mut conn, incoming, true).await != Flow::Stop
        && let Err(err) = writer.shutdown().await
    {
        tracing::debug!(error = %err, "guest went away before the close");
    }
}

/// Sends `incoming` upstream on `conn`, its body streamed from the guest, and writes the answer
/// back. `last`: the connection's request limit is reached, so the answer says it closes.
async fn exchange<R, W>(
    config: &ProxyConfig,
    guest: &mut Guest<'_, R, W>,
    conn: &mut Kept,
    incoming: &Incoming,
    last: bool,
) -> Flow
where
    R: AsyncBufRead + Unpin + Send,
    W: AsyncWrite + Unpin + Send,
{
    let parsed = &incoming.parsed;
    let request = match upstream_request(incoming, conn.via.as_ref()) {
        Ok(request) => request,
        Err(refusal) => {
            refuse(guest.writer, &refusal).await;
            return Flow::Stop;
        }
    };
    let (request, tx, abort) = request;
    let has_body = incoming.has_body();
    if parsed.expect_continue
        && has_body
        && let Err(err) = guest
            .writer
            .write_all(b"HTTP/1.1 100 Continue\r\n\r\n")
            .await
    {
        tracing::debug!(error = %err, "guest went away before its body");
        return Flow::Stop;
    }
    let (sent, pumped) = h1::send_and_pump(
        &mut conn.conn.sender,
        guest.reader,
        request,
        (parsed.body, tx, abort),
        config,
    )
    .await;
    let body_complete = matches!(pumped, Some(Ok(())));
    let host = &incoming.target.host;
    let response = match sent {
        Some(Ok(response)) => response,
        Some(Err(err)) => {
            let refusal = if let Some(Err(why)) = &pumped {
                tracing::info!(%host, error = %why, "request body failed");
                Refusal::new("400 Bad Request", "the request body could not be read")
            } else {
                tracing::info!(%host, error = %err, "upstream request failed");
                Refusal::new(
                    "502 Bad Gateway",
                    format!("the request to {host} failed: {}", h1::short(&err)),
                )
            };
            refuse(guest.writer, &refusal).await;
            return Flow::Stop;
        }
        None => {
            let refusal = Refusal::new(
                "504 Gateway Timeout",
                format!(
                    "{host} did not answer within {}s",
                    config.upstream_head_timeout.as_secs()
                ),
            );
            refuse(guest.writer, &refusal).await;
            return Flow::Stop;
        }
    };
    let written = tokio::select! {
        written = h1::write_response(
            guest.writer,
            response.map(AnswerBody::Streaming),
            &parsed.method,
            if last {
                parsed.guest().closing()
            } else {
                parsed.guest()
            },
            config.body_idle_timeout,
        ) => written,
        never = h1::keep_reading(guest.reader) => match never {},
    };
    match written {
        // Bytes of a body that was not read to its end would be read as the next request.
        Ok(Written::KeepOpen) if body_complete || !has_body => Flow::Continue,
        Ok(_) => Flow::Close,
        Err(err) => {
            tracing::info!(%host, error = %err, "plain-HTTP response aborted");
            Flow::Stop
        }
    }
}

/// The request to send: the checked method and path (an absolute URI toward a proxy, with the
/// proxy's own credential), the guest's end-to-end headers with `Host` set to the checked target,
/// and a body the guest's framing is decoded into. Hop-by-hop headers, the guest's
/// `Proxy-Authorization` and Puddle's own header names never go upstream.
fn upstream_request(
    incoming: &Incoming,
    via: Option<&ProxyForm>,
) -> Result<(::http::Request<UpBody>, BodySender, Abort), Refusal> {
    let target = &incoming.target;
    let path = &incoming.parsed.target;
    let mut headers = h1::upstream_headers(&incoming.head, target, None)?;
    let host = HeaderValue::from_str(&target.host_header())
        .map_err(|_| Refusal::new("400 Bad Request", "host"))?;
    headers.insert(HOST, host);
    let uri = match via {
        Some(proxy) => {
            if let Some(secret) = proxy.authorization.as_ref() {
                let mut value = HeaderValue::from_str(secret.expose())
                    .map_err(|_| Refusal::new("502 Bad Gateway", "unusable proxy credential"))?;
                value.set_sensitive(true);
                headers.insert(PROXY_AUTHORIZATION, value);
            }
            format!("http://{}{path}", proxy.authority)
        }
        None => path.clone(),
    };
    let (body, tx, abort) = ChannelBody::new(incoming.parsed.body);
    let body: UpBody = body
        .map_err(|err| -> BoxError { Box::new(err) })
        .boxed_unsync();
    let mut builder = ::http::Request::builder()
        .method(incoming.parsed.method.as_str())
        .uri(uri)
        .version(::http::Version::HTTP_11);
    if let Some(map) = builder.headers_mut() {
        *map = headers;
    }
    let request = builder
        .body(body)
        .map_err(|_| Refusal::new("400 Bad Request", "malformed request"))?;
    Ok((request, tx, abort))
}

#[cfg(test)]
mod tests;
