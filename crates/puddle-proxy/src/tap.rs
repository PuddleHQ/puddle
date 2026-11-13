// SPDX-License-Identifier: AGPL-3.0-or-later
//! The first request line inside a `CONNECT` tunnel, for the audit.
//!
//! Node `fetch` (`NODE_USE_ENV_PROXY`) and Yarn Berry send a plain `http://` request as
//! `CONNECT host:80` followed by the request itself, not as an absolute-form request. The tunnel is
//! decided like any other request (the rules see `(workspace, host, port)` either way, R-2), and its
//! bytes are relayed unchanged; [`RequestTap`] only watches the start of the upload so the
//! `connection` record can name the method and path, as it does for an absolute-form request.
//!
//! The tap never waits for bytes, never holds them back and never changes them: a tunnel that
//! doesn't start with an HTTP/1.x request line (TLS, a server-first protocol) behaves exactly
//! as before and is recorded without a request line. (An SSH identification line ends the tunnel
//! instead: [`crate::ssh`].)

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use puddle_types::HttpRequestLine;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use crate::http::is_tchar;

/// Most upload bytes kept while looking for the first request line. A longer line is not
/// recorded (common servers refuse request lines over 8 KiB too).
pub(crate) const MAX_TAPPED: usize = 8 * 1024;

/// A guest stream that keeps a copy of the first upload bytes, up to the end of the first
/// request line or [`MAX_TAPPED`].
#[derive(Debug)]
pub(crate) struct RequestTap<S> {
    inner: S,
    seen: Vec<u8>,
    full: bool,
}

impl<S> RequestTap<S> {
    /// Taps `inner`; `early` are upload bytes already read past the `CONNECT` head.
    pub(crate) fn new(inner: S, early: &[u8]) -> Self {
        let mut tap = Self {
            inner,
            seen: Vec::new(),
            full: false,
        };
        tap.keep(early);
        tap
    }

    fn keep(&mut self, bytes: &[u8]) {
        if self.full {
            return;
        }
        let room = MAX_TAPPED.saturating_sub(self.seen.len());
        self.seen
            .extend_from_slice(bytes.get(..room.min(bytes.len())).unwrap_or_default());
        self.full = self.seen.len() >= MAX_TAPPED || line_end(&self.seen).is_some();
    }

    /// The first request line, if the upload started with a well-formed HTTP/1.x one.
    pub(crate) fn request_line(&self) -> Option<HttpRequestLine> {
        first_request_line(&self.seen)
    }
}

/// Where the first request line ends (its `\n`), after any empty lines before it.
fn line_end(bytes: &[u8]) -> Option<usize> {
    let start = bytes.iter().position(|b| *b != b'\r' && *b != b'\n')?;
    bytes
        .get(start..)?
        .iter()
        .position(|b| *b == b'\n')
        .map(|n| start + n)
}

/// The method and path of `bytes`' first line if it is an HTTP/1.x request line with an
/// origin-form (`/...`) or absolute-form (`http://...`) target. Empty lines before it are skipped
/// (RFC 9112 §2.2). The path loses its query string (`HttpRequestLine`, R-25).
pub(crate) fn first_request_line(bytes: &[u8]) -> Option<HttpRequestLine> {
    let end = line_end(bytes)?;
    let start = bytes.iter().position(|b| *b != b'\r' && *b != b'\n')?;
    let line = bytes.get(start..end)?;
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    if line.iter().any(|b| !(0x21..0x7f).contains(b) && *b != b' ') {
        return None;
    }
    let line = std::str::from_utf8(line).ok()?;
    let mut parts = line.split(' ');
    let (Some(method), Some(target), Some(version), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return None;
    };
    let method_ok = !method.is_empty() && method.bytes().all(is_tchar);
    let target_ok = target.starts_with('/')
        || target
            .get(..7)
            .is_some_and(|scheme| scheme.eq_ignore_ascii_case("http://"));
    let version_ok = version == "HTTP/1.1" || version == "HTTP/1.0";
    (method_ok && target_ok && version_ok).then(|| HttpRequestLine::new(method, target))
}

impl<S: AsyncRead + Unpin> AsyncRead for RequestTap<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let before = buf.filled().len();
        let polled = Pin::new(&mut self.inner).poll_read(cx, buf);
        if !self.full
            && let Poll::Ready(Ok(())) = polled
        {
            let new = buf.filled().get(before..).unwrap_or_default().to_vec();
            self.keep(&new);
        }
        polled
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for RequestTap<S> {
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

#[cfg(test)]
mod tests;
