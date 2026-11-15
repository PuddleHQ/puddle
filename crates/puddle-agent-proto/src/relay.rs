// SPDX-License-Identifier: AGPL-3.0-or-later
//! Splicing a TCP connection to a yamux stream so that an abort stays an abort.
//!
//! If one side of a proxied connection fails (the client aborted its upload, the server reset, or
//! the session died), the other side must see a **reset**, not a clean close: a FIN would make a
//! cut-off upload or download look complete to a peer that reads until close.
//!
//! - The TCP side gets RST because [`splice`] sets zero linger on it before it is dropped.
//! - The yamux side gets RST because a stream dropped without a shutdown sends one. Callers
//!   therefore **drop** the stream after an error, never shut it down.
//!
//! The agent splices the guest application's TCP connection; the host splices the server's.

use std::io;

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;

/// Copies both ways between `tcp` and `other` until both sides are done. A side that half-closes
/// (sends its request and shuts down its write half) still gets a response of any size: the
/// vendored yamux keeps receiving window updates after the peer's end of stream. Returns the bytes
/// copied `(tcp → other, other → tcp)`.
///
/// # Errors
///
/// The first I/O error on either side. Before returning it, `tcp` is set to zero linger, so
/// dropping it sends a TCP reset; drop `other` without shutting it down so it resets too.
pub async fn splice<S>(tcp: &mut TcpStream, other: &mut S) -> io::Result<(u64, u64)>
where
    S: AsyncRead + AsyncWrite + Unpin + ?Sized,
{
    match tokio::io::copy_bidirectional(tcp, other).await {
        Ok(counts) => Ok(counts),
        Err(err) => {
            if let Err(linger) = tcp.set_zero_linger() {
                // The socket is already gone; there is nothing left to reset.
                tracing::debug!(error = %linger, "zero linger after a failed splice");
            }
            Err(err)
        }
    }
}

#[cfg(test)]
mod tests;
