// SPDX-License-Identifier: GPL-3.0-or-later
//! The real route to a Git host: Puddle's own connection through the company proxy chain, TLS
//! checked the way the host's own programs check it, then one HTTP/1.1 request.
//!
//! A connection that answered stays open for a moment and serves the next request to the same host
//! (the pages of one list, a list and a profile): opening one through a company proxy costs a
//! sign-in and a TLS handshake, which a long list would pay for every page.
//!
//! Nothing is guarded the way a workspace's request is: the destination is the credential's own
//! host, chosen by the user. A redirect is never followed, so the token goes to that host and
//! nowhere else, and an answer is read up to [`MAX_BODY`] bytes.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use bytes::Bytes;
use futures_util::future::BoxFuture;
use http::header::{ACCEPT, AUTHORIZATION, HOST, IF_NONE_MATCH, USER_AGENT};
use http::{HeaderValue, Request};
use http_body_util::{BodyExt, Empty, Limited};
use hyper::client::conn::http1::SendRequest;
use hyper_util::rt::TokioIo;
use puddle_types::{
    ConnectionDecision, ConnectionEvent, ConnectionLog, ConnectionReason, Host, HttpRequestLine,
    NullConnectionLog,
};
use puddle_upstream::host::connect;
use puddle_upstream::{Chain, Destination, Form, Scheme, TlsClient, TlsConnectError};
use tokio::time::Instant;

use crate::api::{Api, ApiReply, ApiRequest, MAX_BODY, TransportError};

/// The time one request may take, connection and TLS included.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// How long an answered connection waits for the next request before it is not used again. Hosts
/// close idle connections after a minute or so; shorter keeps the first request on one from
/// finding it closed.
const IDLE_FOR: Duration = Duration::from_secs(15);

/// Most idle connections kept for one host: a read is four lists at once at most.
const MAX_IDLE_PER_HOST: usize = 4;

/// A connection that answered and may answer again.
struct Idle {
    sender: SendRequest<Empty<Bytes>>,
    since: Instant,
}

/// Requests over Puddle's own route. Cheap to share.
pub struct HostApi {
    chain: Arc<Chain>,
    tls: TlsClient,
    timeout: Duration,
    log: Arc<dyn ConnectionLog>,
    /// Answered connections by `host[:port]`, newest last.
    idle: Mutex<HashMap<String, Vec<Idle>>>,
    idle_for: Duration,
}

impl std::fmt::Debug for HostApi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostApi").finish_non_exhaustive()
    }
}

impl HostApi {
    /// Requests that leave through `chain` (the company proxy route) and trust what `tls` trusts.
    #[must_use]
    pub fn new(chain: Arc<Chain>, tls: TlsClient) -> Self {
        Self {
            chain,
            tls,
            timeout: REQUEST_TIMEOUT,
            log: Arc::new(NullConnectionLog),
            idle: Mutex::new(HashMap::new()),
            idle_for: IDLE_FOR,
        }
    }

    /// The same, recording every request in `log` as Puddle's own connection (origin `puddle`,
    /// the Git host, and whether it worked), like an image pull.
    #[must_use]
    pub fn with_connection_log(mut self, log: Arc<dyn ConnectionLog>) -> Self {
        self.log = log;
        self
    }

    /// The same with another limit for one request.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// The same with another time an answered connection waits for its next request.
    #[must_use]
    pub fn with_idle_time(mut self, idle_for: Duration) -> Self {
        self.idle_for = idle_for;
        self
    }

    async fn exchange(&self, request: ApiRequest) -> Result<ApiReply, TransportError> {
        let authorization = request
            .authorization
            .header_value()
            .ok_or(TransportError::BadToken)?;
        // A kept connection the host closed meanwhile fails before it answers anything: a `GET`
        // is safe to send again, once, on a new connection.
        if let Some(mut sender) = self.take_idle(&request.host) {
            match send(&mut sender, &request, authorization.clone()).await {
                Ok(reply) => {
                    self.keep(&request.host, sender, &reply);
                    return Ok(reply);
                }
                Err(TransportError::Protocol(_)) => {}
                Err(other) => return Err(other),
            }
        }
        let mut sender = self.open(&request.host).await?;
        let reply = send(&mut sender, &request, authorization).await?;
        self.keep(&request.host, sender, &reply);
        Ok(reply)
    }

    /// A new connection to `host` (through the chain, TLS checked), served by its own task.
    async fn open(&self, host: &str) -> Result<SendRequest<Empty<Bytes>>, TransportError> {
        let (name, port) = split_port(host);
        let destination = Destination::new(Scheme::Https, name, port.unwrap_or(443));
        let connected = connect(&self.chain, &destination, Form::Tunnel)
            .await
            .map_err(|err| TransportError::Unreachable(err.to_string()))?;
        let stream = self
            .tls
            .connect(name, connected.stream)
            .await
            .map_err(|err| match err {
                TlsConnectError::InvalidName(name) => {
                    TransportError::Unreachable(format!("{name:?} is not a valid server name"))
                }
                TlsConnectError::Certificate(why) => TransportError::Tls(why),
                other => TransportError::Unreachable(format!("the TLS handshake failed: {other}")),
            })?;
        let (sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
            .await
            .map_err(protocol)?;
        // Ends when the sender is dropped or the host closes.
        tokio::spawn(async move {
            let _ = connection.await;
        });
        Ok(sender)
    }

    fn idle(&self) -> std::sync::MutexGuard<'_, HashMap<String, Vec<Idle>>> {
        self.idle.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The newest connection to `host` that is still open and not idle for too long.
    fn take_idle(&self, host: &str) -> Option<SendRequest<Empty<Bytes>>> {
        let mut idle = self.idle();
        let kept = idle.get_mut(host)?;
        let found = std::iter::from_fn(|| kept.pop())
            .find(|c| !c.sender.is_closed() && c.since.elapsed() < self.idle_for)
            .map(|c| c.sender);
        if kept.is_empty() {
            idle.remove(host);
        }
        found
    }

    /// Keeps `sender` for the next request to `host`, unless the host said it closes after this
    /// answer. Connections that went stale are dropped here, which closes them.
    fn keep(&self, host: &str, sender: SendRequest<Empty<Bytes>>, reply: &ApiReply) {
        let closing = reply
            .header("connection")
            .is_some_and(|v| v.eq_ignore_ascii_case("close"));
        if closing || sender.is_closed() {
            return;
        }
        let mut idle = self.idle();
        idle.retain(|_, kept| {
            kept.retain(|c| !c.sender.is_closed() && c.since.elapsed() < self.idle_for);
            !kept.is_empty()
        });
        let kept = idle.entry(host.to_owned()).or_default();
        if kept.len() >= MAX_IDLE_PER_HOST {
            kept.remove(0);
        }
        kept.push(Idle {
            sender,
            since: Instant::now(),
        });
    }
}

impl Api for HostApi {
    fn get(&self, request: ApiRequest) -> BoxFuture<'_, Result<ApiReply, TransportError>> {
        Box::pin(async move {
            let (host, path) = (request.host.clone(), request.path.clone());
            let result = tokio::time::timeout(self.timeout, self.exchange(request))
                .await
                .unwrap_or(Err(TransportError::Timeout(self.timeout.as_secs().max(1))));
            self.record(&host, &path, &result);
            result
        })
    }
}

impl HostApi {
    /// Writes the request as a `connection` record of Puddle's own. A write that fails is the
    /// log's to report; the listing is not affected.
    fn record(&self, host: &str, path: &str, result: &Result<ApiReply, TransportError>) {
        let (name, port) = split_port(host);
        let Ok(name) = Host::parse_normalised(name) else {
            tracing::warn!(
                host,
                "a repository listing request is not in the activity log: the host name is not valid"
            );
            return;
        };
        let (reason, down) = match result {
            Ok(reply) if reply.status < 400 => (ConnectionReason::PuddleRequest, reply.body.len()),
            Ok(reply) => (
                ConnectionReason::PuddleRequestFailed(match reply.status {
                    401 | 403 => "host_refused",
                    429 => "rate_limited",
                    _ => "host_error",
                }),
                reply.body.len(),
            ),
            Err(err) => (ConnectionReason::PuddleRequestFailed(failure_code(err)), 0),
        };
        let mut event =
            ConnectionEvent::puddle(name, port.unwrap_or(443), ConnectionDecision::Allow, reason);
        event.http = Some(HttpRequestLine::new("GET", path));
        event.bytes_down = down as u64;
        self.log.record(&event);
    }
}

/// The audit code of a request that got no answer.
fn failure_code(err: &TransportError) -> &'static str {
    match err {
        TransportError::Unreachable(_) => "unreachable",
        TransportError::Tls(_) => "tls",
        TransportError::Timeout(_) => "timeout",
        TransportError::TooLarge => "too_large",
        TransportError::Protocol(_) => "protocol",
        TransportError::BadToken => "bad_token",
    }
}

/// `host[:port]` as the name and the port when there is one.
fn split_port(host: &str) -> (&str, Option<u16>) {
    match host.rsplit_once(':') {
        Some((name, port)) => port.parse().map_or((host, None), |p| (name, Some(p))),
        None => (host, None),
    }
}

fn protocol(err: impl std::fmt::Display) -> TransportError {
    TransportError::Protocol(err.to_string())
}

/// One `GET` on `sender`; the connection can serve another once the answer is read.
async fn send(
    sender: &mut SendRequest<Empty<Bytes>>,
    request: &ApiRequest,
    authorization: HeaderValue,
) -> Result<ApiReply, TransportError> {
    sender.ready().await.map_err(protocol)?;
    let mut builder = Request::builder()
        .method("GET")
        .uri(&request.path)
        .header(HOST, &request.host)
        .header(USER_AGENT, "puddle")
        .header(ACCEPT, request.accept)
        .header(AUTHORIZATION, authorization);
    for (name, value) in request.headers {
        builder = builder.header(*name, *value);
    }
    if let Some(etag) = &request.if_none_match {
        builder = builder.header(IF_NONE_MATCH, etag);
    }
    let request = builder.body(Empty::<Bytes>::new()).map_err(protocol)?;
    let response = sender.send_request(request).await.map_err(protocol)?;
    let (parts, body) = response.into_parts();
    let body = Limited::new(body, MAX_BODY)
        .collect()
        .await
        .map_err(|err| {
            if err.is::<http_body_util::LengthLimitError>() {
                TransportError::TooLarge
            } else {
                protocol(err)
            }
        })?
        .to_bytes();
    let mut headers = std::collections::BTreeMap::new();
    for (name, value) in &parts.headers {
        if let Ok(value) = value.to_str() {
            headers
                .entry(name.as_str().to_owned())
                .or_insert_with(|| value.to_owned());
        }
    }
    Ok(ApiReply {
        status: parts.status.as_u16(),
        headers,
        body: body.to_vec(),
    })
}
