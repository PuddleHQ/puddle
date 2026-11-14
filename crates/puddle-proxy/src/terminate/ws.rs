// SPDX-License-Identifier: AGPL-3.0-or-later
//! WebSocket through a terminated connection (RFC 6455 over HTTP/1.1 `Upgrade`, RFC 8441 over
//! HTTP/2 extended `CONNECT`).
//!
//! The proxy checks and injects the handshake request like any other request. When the server
//! agrees (`101` on HTTP/1.1, `2xx` on HTTP/2), the two connections become a byte pipe: what the
//! frames mean is none of the proxy's business.

use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context as TaskContext, Poll};
use std::time::Duration;

use ::http::header::{HeaderMap, HeaderName, HeaderValue};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use hyper::upgrade::Upgraded;
use hyper_util::rt::TokioIo;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use super::streams::Activity;

/// The GUID RFC 6455 §1.3 mixes into the key.
const GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// A fresh `Sec-WebSocket-Key` (16 random bytes, base64).
pub(crate) fn new_key() -> Option<String> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).ok()?;
    Some(STANDARD.encode(bytes))
}

/// The `Sec-WebSocket-Accept` a server answers `key` with.
pub(crate) fn accept_for(key: &str) -> String {
    let mut input = Vec::with_capacity(key.len() + GUID.len());
    input.extend_from_slice(key.as_bytes());
    input.extend_from_slice(GUID.as_bytes());
    let digest = ring::digest::digest(&ring::digest::SHA1_FOR_LEGACY_USE_ONLY, &input);
    STANDARD.encode(digest.as_ref())
}

/// The headers of the server's handshake answer worth passing to the guest: what is end to end
/// (`Sec-WebSocket-Protocol`, `-Extensions`, `-Accept`, cookies), not the connection management
/// the proxy writes itself.
pub(crate) fn answer_headers(
    headers: &HeaderMap,
    keep_accept: bool,
) -> Vec<(HeaderName, HeaderValue)> {
    headers
        .iter()
        .filter(|(name, _)| {
            !matches!(
                name.as_str(),
                "connection"
                    | "upgrade"
                    | "keep-alive"
                    | "proxy-connection"
                    | "proxy-authenticate"
                    | "te"
                    | "trailer"
                    | "transfer-encoding"
                    | "content-length"
            ) && (keep_accept || **name != HeaderName::from_static("sec-websocket-accept"))
        })
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect()
}

/// Whether `headers` ask for a WebSocket upgrade on HTTP/1.1: `Connection: upgrade` with exactly
/// one `Upgrade: websocket`.
pub(crate) fn is_upgrade_request(connection_tokens: &[String], upgrade_values: &[&str]) -> bool {
    connection_tokens.iter().any(|token| token == "upgrade")
        && matches!(upgrade_values, [one] if one.trim().eq_ignore_ascii_case("websocket"))
}

/// Pipes the guest's side to the server's until either ends, or until no byte has moved in either
/// direction for `idle` (`true`): a pipe has no protocol the proxy reads, so silence is the only
/// sign of a peer that is gone without a close. `activity` is touched by every byte.
pub(crate) async fn splice<G>(
    guest: &mut G,
    upstream: Upgraded,
    activity: &Arc<Activity>,
    idle: Duration,
) -> bool
where
    G: AsyncRead + AsyncWrite + Unpin,
{
    let mut guest = Moving::new(guest, activity);
    let mut upstream = Moving::new(TokioIo::new(upstream), activity);
    let copy = tokio::io::copy_bidirectional(&mut guest, &mut upstream);
    let silent = async {
        loop {
            let quiet = activity.quiet_for();
            if quiet >= idle {
                return;
            }
            tokio::time::sleep(idle.saturating_sub(quiet)).await;
        }
    };
    tokio::select! {
        ended = copy => {
            // the error is logged just above; the answer is only whether to go on
            let _ = ended.inspect_err(|err| tracing::debug!(error = %err, "WebSocket ended with an error"));
            false
        }
        () = silent => {
            activity.note_limit(super::streams::ended::WEBSOCKET_IDLE);
            tracing::info!(idle_secs = idle.as_secs(), "a WebSocket moved no byte for too long and was closed");
            true
        }
    }
}

/// A stream that notes every byte that passes through it.
struct Moving<S> {
    inner: S,
    activity: Arc<Activity>,
}

impl<S> Moving<S> {
    fn new(inner: S, activity: &Arc<Activity>) -> Self {
        Self {
            inner,
            activity: Arc::clone(activity),
        }
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for Moving<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let before = buf.filled().len();
        let polled = Pin::new(&mut self.inner).poll_read(cx, buf);
        if matches!(polled, Poll::Ready(Ok(()))) && buf.filled().len() > before {
            self.activity.touch();
        }
        polled
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Moving<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let polled = Pin::new(&mut self.inner).poll_write(cx, buf);
        if matches!(polled, Poll::Ready(Ok(n)) if n > 0) {
            self.activity.touch();
        }
        polled
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

/// The header `name` of `headers` as text, if there is exactly one.
pub(crate) fn single<'a>(headers: &'a HeaderMap, name: &HeaderName) -> Option<&'a str> {
    let mut all = headers.get_all(name).iter();
    let first = all.next()?;
    if all.next().is_some() {
        return None;
    }
    first.to_str().ok()
}

/// `Sec-WebSocket-Version` the proxy can translate (RFC 6455 §4.1).
pub(crate) fn version_ok(headers: &HeaderMap) -> bool {
    single(headers, &HeaderName::from_static("sec-websocket-version")) == Some("13")
}

/// Whether the answer's `Sec-WebSocket-Accept` is the one for `key`.
pub(crate) fn accept_matches(headers: &HeaderMap, key: &str) -> bool {
    single(headers, &HeaderName::from_static("sec-websocket-accept"))
        .is_some_and(|got| got == accept_for(key))
}

#[cfg(test)]
mod tests;
