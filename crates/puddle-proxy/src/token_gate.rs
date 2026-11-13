// SPDX-License-Identifier: AGPL-3.0-or-later
//! Refusing a tunnel that is not decrypted but asks, in its TLS `ClientHello`, for a token
//! endpoint of the workspace's logins.
//!
//! A login's refresh token and authorization code go to its token endpoint, and the proxy swaps
//! them there only on a decrypted connection. A guest that `CONNECT`s to something else (another
//! name, an IP address, another port) and then asks for the token endpoint by name would send
//! them to the real server unswapped, which for a careless tool is a stand-in that does not work
//! and for a login made with capture on a real code. [`TokenGate`] reads the first upload bytes
//! and ends the tunnel when the name is such an endpoint.
//!
//! It narrows the careless case only. Encrypted `ClientHello`, no name at all, and a name that
//! differs from the `Host` of a request inside the TLS (domain fronting) are not seen.

use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use crate::terminate::{Scan, Termination, scan_hello};

/// The audit code of a tunnel this refuses.
pub(crate) const CODE: &str = "token_endpoint_spliced";

/// How much of the upload is read while looking for the end of the hello; a longer one is not a
/// hello any client sends.
const MAX_SEEN: usize = 16 * 1024;

/// A fatal TLS alert (`handshake_failure`), what the guest's client gets instead of a handshake.
pub(crate) const ALERT: [u8; 7] = [0x15, 0x03, 0x03, 0x00, 0x02, 0x02, 0x28];

/// A guest stream that refuses to pass on a `ClientHello` for a token endpoint.
#[derive(Debug)]
pub(crate) struct TokenGate<S> {
    inner: S,
    termination: Option<Arc<Termination>>,
    seen: Vec<u8>,
    refused: bool,
}

impl<S> TokenGate<S> {
    /// Wraps `inner`. Without `termination` (or when it reads no token exchanges) the gate only
    /// passes bytes on. `early` is what the client already sent; it is checked at once.
    pub(crate) fn new(inner: S, termination: Option<Arc<Termination>>, early: &[u8]) -> Self {
        let mut gate = Self {
            inner,
            termination,
            seen: Vec::new(),
            refused: false,
        };
        if !early.is_empty() {
            gate.look(early);
        }
        gate
    }

    /// Whether the hello asked for a token endpoint (its last bytes were dropped).
    pub(crate) fn refused(&self) -> bool {
        self.refused
    }

    /// The stream under the gate.
    pub(crate) fn inner(&self) -> &S {
        &self.inner
    }

    /// Adds `bytes` to what was seen; stops watching once the hello is known to be complete or
    /// not a hello.
    fn look(&mut self, bytes: &[u8]) {
        let Some(termination) = &self.termination else {
            return;
        };
        self.seen.extend_from_slice(bytes);
        match scan_hello(&self.seen) {
            Scan::More if self.seen.len() < MAX_SEEN => return,
            Scan::Name(Some(host)) if termination.is_token_endpoint(&host) => self.refused = true,
            Scan::More | Scan::Other | Scan::Name(_) => {}
        }
        self.termination = None;
        self.seen = Vec::new();
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for TokenGate<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if self.refused {
            return Poll::Ready(Err(refusal()));
        }
        let before = buf.filled().len();
        let polled = Pin::new(&mut self.inner).poll_read(cx, buf);
        if let Poll::Ready(Ok(())) = polled
            && self.termination.is_some()
        {
            let chunk = buf.filled().get(before..).unwrap_or_default().to_vec();
            self.look(&chunk);
            if self.refused {
                buf.set_filled(before);
                return Poll::Ready(Err(refusal()));
            }
        }
        polled
    }
}

fn refusal() -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, CODE)
}

impl<S: AsyncWrite + Unpin> AsyncWrite for TokenGate<S> {
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
