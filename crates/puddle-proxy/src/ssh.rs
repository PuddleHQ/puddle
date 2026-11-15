// SPDX-License-Identifier: AGPL-3.0-or-later
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
//! The gate holds back only the first bytes of the upload, those sent with the request included,
//! and only while they could still begin a banner (`S`, `SS`, `SSH`, `SSH-`): a client that splits
//! its identification line over several writes is caught like one that sends it whole. Every byte
//! that turns out not to start a banner is passed on unchanged and in order.
//!
//! The header is the guest's word, so it only ever makes the proxy stricter. Where it would make a
//! connection to a decrypted name a plain splice instead, the bytes decide ([`undecided`]).

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

/// Whether the first bytes of an upload do not tell yet if it is SSH: all of them are the start
/// of `SSH-` (so fewer than `BANNER_MIN`). Nothing at all is undecided too.
pub(crate) fn undecided(first: &[u8]) -> bool {
    b"SSH-".starts_with(first)
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
    /// Upload bytes taken but not handed on yet: those sent with the request, then (while
    /// watching) what follows them until the start tells.
    held: Vec<u8>,
    /// How many of `held` were handed on.
    handed: usize,
    watching: bool,
    ssh: bool,
    kex: Option<crate::ssh_agent::KexWatch>,
}

impl<S> Gate<S> {
    /// Wraps `inner`, whose upload starts with `early` (what the client sent with its request; the
    /// gate hands it on first). With `watching`, the start of the upload is checked for a banner.
    pub(crate) fn new(inner: S, early: Vec<u8>, watching: bool) -> Self {
        Self {
            inner,
            held: early,
            handed: 0,
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

    /// Whether the upload started with an SSH identification line (it was dropped).
    pub(crate) fn ssh_seen(&self) -> bool {
        self.ssh
    }

    /// The stream under the gate.
    pub(crate) fn inner(&self) -> &S {
        &self.inner
    }
}

impl<S: AsyncRead + Unpin> Gate<S> {
    /// Reads until the start of the upload tells whether it is a banner, or the stream ends.
    /// `false`: the stream ended before any byte, and the gate goes on watching.
    fn poll_decide(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<bool>> {
        while undecided(&self.held) {
            let mut chunk = [0u8; 1024];
            let mut more = ReadBuf::new(&mut chunk);
            std::task::ready!(Pin::new(&mut self.inner).poll_read(cx, &mut more))?;
            if more.filled().is_empty() {
                if self.held.is_empty() {
                    return Poll::Ready(Ok(false));
                }
                break; // the end after a prefix of `SSH-`: what came is handed on
            }
            self.held.extend_from_slice(more.filled());
        }
        self.watching = false;
        if is_banner(&self.held) {
            self.held = Vec::new();
            self.ssh = true;
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                BANNER_SEEN,
            )));
        }
        Poll::Ready(Ok(true))
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for Gate<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = &mut *self;
        if this.watching && !std::task::ready!(this.poll_decide(cx))? {
            return Poll::Ready(Ok(())); // the end, passed up
        }
        let rest = this.held.get(this.handed..).unwrap_or_default();
        if rest.is_empty() {
            return Pin::new(&mut this.inner).poll_read(cx, buf);
        }
        let n = rest.len().min(buf.remaining());
        buf.put_slice(rest.get(..n).unwrap_or_default());
        this.handed += n;
        if this.handed == this.held.len() {
            this.held = Vec::new();
            this.handed = 0;
        }
        Poll::Ready(Ok(()))
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
