// SPDX-License-Identifier: GPL-3.0-or-later
//! Recognising SSH on a `CONNECT` tunnel and refusing it for Git hosts.
//!
//! SSH to the name of a Git host (`github.com`, `ssh.dev.azure.com`, a host of one of the
//! workspace's Git credentials) that Puddle does not serve is refused at once, whatever the port:
//! the client's identification line (`SSH-2.0-...`) is what tells SSH from anything else
//! ([`puddle_agent_proto::ssh`]). SSH to any other name is decided by the rules like every
//! connection and spliced when allowed, so nothing here touches it. Two places can see the line:
//!
//! - **Before the decision:** the `CONNECT` carries it. The agent's `connect` command sets the
//!   [`PROTOCOL_HEADER`] once it has read the banner, and a client may send the banner right behind
//!   the head. [`announced`] and [`started`] find either, so the request is blocked before the
//!   rules, the resolver or the inbox hear of it.
//! - **In an established tunnel:** a client that waits for the `200` (an SSH client behind a
//!   proxy helper that speaks `CONNECT`, a captured connection) has been allowed through by a rule.
//!   [`Gate`] looks at the first bytes of its upload and, on a banner, ends the tunnel with a
//!   message instead of forwarding them: the server never gets the client's identification.
//!
//! The gate never holds anything back otherwise: it looks at the first chunk the guest sends, once,
//! and passes every byte on unchanged.

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use puddle_agent_proto::origin::{CONTAINER, ORIGIN_HEADER};
use puddle_agent_proto::ssh::{PROTOCOL_HEADER, SSH, is_banner};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use crate::http::{Head, header_name, header_value};

/// Whether the request says the tunnel carries SSH.
pub(crate) fn announced(head: &Head) -> bool {
    head.headers
        .iter()
        .any(|h| header_name(h) == PROTOCOL_HEADER && header_value(h).eq_ignore_ascii_case(SSH))
}

/// Whether the request says its client is in a container (see [`puddle_agent_proto::origin`]).
pub(crate) fn from_container(head: &Head) -> bool {
    head.headers
        .iter()
        .any(|h| header_name(h) == ORIGIN_HEADER && header_value(h).eq_ignore_ascii_case(CONTAINER))
}

/// Whether the bytes the client sent with the request head are an SSH identification line.
pub(crate) fn started(early: &[u8]) -> bool {
    is_banner(early)
}

/// What the guest is told when an established tunnel turns out to carry SSH: the refusal as
/// lines an SSH client skips before the server's own identification (RFC 4253 section 4.2: lines
/// that don't start with `SSH-`), so a client that shows them (`ssh -v`) shows the reason.
pub(crate) fn pre_banner(message: &str) -> Vec<u8> {
    format!("puddle: {message}\r\n").into_bytes()
}

/// The error the [`Gate`] fails its first read with when the upload starts with a banner.
const BANNER_SEEN: &str = "SSH identification line";

/// A guest stream that refuses to pass on an upload that starts with an SSH identification line.
#[derive(Debug)]
pub(crate) struct Gate<S> {
    inner: S,
    watching: bool,
    ssh: bool,
    kex: Option<crate::ssh_agent::KexWatch>,
}

impl<S> Gate<S> {
    /// Wraps `inner`; with `watching` its first upload bytes are checked, otherwise it only
    /// passes bytes on (the client already sent some with its request, so they are not the start).
    pub(crate) fn new(inner: S, watching: bool) -> Self {
        Self {
            inner,
            watching,
            ssh: false,
            kex: None,
        }
    }

    /// Also shows the bytes written toward the guest (the server's side of the tunnel) to `kex`,
    /// which keeps the session's key exchange for the filtered SSH agent while the gate lives.
    pub(crate) fn with_kex(mut self, kex: Option<crate::ssh_agent::KexWatch>) -> Self {
        self.kex = kex;
        self
    }

    /// Whether the first upload bytes were an SSH identification line (they were dropped).
    pub(crate) fn ssh_seen(&self) -> bool {
        self.ssh
    }

    /// The stream under the gate.
    pub(crate) fn inner(&self) -> &S {
        &self.inner
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for Gate<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let before = buf.filled().len();
        let polled = Pin::new(&mut self.inner).poll_read(cx, buf);
        if self.watching
            && let Poll::Ready(Ok(())) = polled
            && buf.filled().len() > before
        {
            self.watching = false;
            if is_banner(buf.filled().get(before..).unwrap_or_default()) {
                buf.set_filled(before);
                self.ssh = true;
                return Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    BANNER_SEEN,
                )));
            }
        }
        polled
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Gate<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let polled = Pin::new(&mut self.inner).poll_write(cx, buf);
        if let (Poll::Ready(Ok(written)), Some(kex)) = (&polled, self.kex.as_mut()) {
            kex.feed(buf.get(..*written).unwrap_or_default());
        }
        polled
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
