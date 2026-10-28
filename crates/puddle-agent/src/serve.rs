// SPDX-License-Identifier: GPL-3.0-or-later
//! The proxy listener inside the guest: every accepted connection becomes one yamux stream to
//! the host, spliced so an abort stays an abort. The agent doesn't parse the request;
//! the host proxy does, except for one thing: a request from a container (see
//! `crate::origin`) gets [`ORIGIN_HEADER`] after its first line, so the proxy does not decrypt it.

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use puddle_agent_proto::origin::{CONTAINER, ORIGIN_HEADER};
use puddle_agent_proto::relay::splice;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpSocket, TcpStream};
use tokio::task::JoinSet;

use crate::upstream::Upstream;

/// Pause after a failed accept (e.g. out of file descriptors) so the loop doesn't spin.
const ACCEPT_PAUSE: Duration = Duration::from_millis(50);

/// Connections the kernel may hold for the accept loop before it starts dropping handshakes (the
/// kernel clamps this to `net.core.somaxconn`, 4096 on current kernels). A build tool opens
/// thousands at once, and tokio's own default of 128 overflows: the dropped handshakes come back
/// as retransmits seconds later or, rarely, as a reset.
pub const LISTEN_BACKLOG: u32 = 4096;

/// How long a container's client gets to send its request line before the connection goes up
/// unmarked.
const REQUEST_LINE_WAIT: Duration = Duration::from_secs(10);

/// The longest request line looked for.
const REQUEST_LINE_MAX: usize = 16 * 1024;

/// Binds `addr` like [`TcpListener::bind`], but with [`LISTEN_BACKLOG`] instead of tokio's 128.
///
/// # Errors
///
/// Creating, binding or listening on the socket failing.
pub fn listen(addr: SocketAddr) -> io::Result<TcpListener> {
    let socket = match addr {
        SocketAddr::V4(_) => TcpSocket::new_v4()?,
        SocketAddr::V6(_) => TcpSocket::new_v6()?,
    };
    // What `TcpListener::bind` does: a restart must not wait out the old socket's TIME_WAIT.
    #[cfg(unix)]
    socket.set_reuseaddr(true)?;
    socket.bind(addr)?;
    socket.listen(LISTEN_BACKLOG)
}

/// Accepts guest connections forever. Connection tasks are owned here and end with it.
pub async fn serve(listener: TcpListener, upstream: Arc<Upstream>) {
    let mut conns = JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((client, _)) => {
                    conns.spawn(connection(client, Arc::clone(&upstream)));
                }
                Err(err) => {
                    tracing::warn!(error = %err, "accept failed");
                    tokio::time::sleep(ACCEPT_PAUSE).await;
                }
            },
            Some(_) = conns.join_next(), if !conns.is_empty() => {}
        }
    }
}

/// What a container's client sent first: up to and including its request line, plus whatever came
/// in the same reads. Ends early (with what there is) on a closed connection, a pause over
/// [`REQUEST_LINE_WAIT`] or a line over [`REQUEST_LINE_MAX`].
async fn request_start<R: AsyncRead + Unpin>(client: &mut R) -> Vec<u8> {
    let mut seen = Vec::new();
    let mut chunk = [0u8; 1024];
    let deadline = tokio::time::Instant::now() + REQUEST_LINE_WAIT;
    while !seen.contains(&b'\n') && seen.len() < REQUEST_LINE_MAX {
        match tokio::time::timeout_at(deadline, client.read(&mut chunk)).await {
            Ok(Ok(n)) if n > 0 => seen.extend_from_slice(chunk.get(..n).unwrap_or_default()),
            _ => break,
        }
    }
    seen
}

/// `first` with the origin header after its request line. Only a `CONNECT` is marked: the proxy
/// decides about decrypting at the tunnel, and a plain request is forwarded to its server, which
/// must not see the header. Anything else is returned as it is.
fn mark_request(first: &[u8]) -> Vec<u8> {
    let Some(end) = first.iter().position(|b| *b == b'\n') else {
        return first.to_vec();
    };
    let (line, rest) = first.split_at(end + 1);
    let text = line.trim_ascii_end();
    let http1 = text.ends_with(b" HTTP/1.1") || text.ends_with(b" HTTP/1.0");
    if !(text.starts_with(b"CONNECT ") && http1) {
        return first.to_vec();
    }
    let mut out = line.to_vec();
    out.extend_from_slice(format!("{ORIGIN_HEADER}: {CONTAINER}\r\n").as_bytes());
    out.extend_from_slice(rest);
    out
}

/// Whether `client` is in a container (see [`crate::origin`]; only Linux guests have them).
fn in_container(client: &TcpStream) -> bool {
    #[cfg(target_os = "linux")]
    {
        client.peer_addr().is_ok_and(crate::origin::from_container)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = client;
        false
    }
}

/// Reads a container's request start and sends it to the host with the origin header added.
async fn send_marked<R, W>(client: &mut R, stream: &mut W) -> io::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let first = request_start(client).await;
    stream
        .write_all(&mark_request(&first))
        .await
        .inspect_err(|err| tracing::debug!(error = %err, "writing a container's request failed"))
}

async fn connection(mut client: TcpStream, upstream: Arc<Upstream>) {
    let mut stream = match upstream.open().await {
        Ok(stream) => stream,
        Err(err) => {
            tracing::warn!(error = %err, "no stream to the host; connection reset");
            // A reset, so the client sees a failure rather than an empty response.
            let _ = client.set_zero_linger();
            return;
        }
    };
    if in_container(&client) && send_marked(&mut client, &mut stream).await.is_err() {
        return;
    }
    if let Err(err) = splice(&mut client, &mut stream).await {
        // The stream is dropped without a shutdown below, which resets it on the host side.
        tracing::debug!(error = %err, "guest connection ended with an error");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_line_gets_the_origin_header_right_after_it() {
        let marked = mark_request(
            b"CONNECT registry.example:443 HTTP/1.1\r\nHost: registry.example:443\r\n\r\n",
        );
        assert_eq!(
            marked,
            b"CONNECT registry.example:443 HTTP/1.1\r\nx-puddle-origin: container\r\nHost: registry.example:443\r\n\r\n"
        );
        // A bare line feed ends a request line too, and what follows the line is kept.
        assert_eq!(
            mark_request(b"CONNECT a.example:443 HTTP/1.0\nHost: a.example\n\n"),
            b"CONNECT a.example:443 HTTP/1.0\nx-puddle-origin: container\r\nHost: a.example\n\n"
        );
    }

    #[tokio::test]
    async fn a_request_line_that_arrives_in_pieces_is_marked_and_what_follows_it_is_kept() {
        let (mut client, mut guest) = tokio::io::duplex(64);
        let sender = tokio::spawn(async move {
            guest.write_all(b"CONNECT a.example:4").await.unwrap();
            tokio::task::yield_now().await;
            guest
                .write_all(b"43 HTTP/1.1\r\nHost: a.example\r\n\r\n")
                .await
                .unwrap();
        });
        let mut up = Vec::new();
        send_marked(&mut client, &mut up).await.unwrap();
        sender.await.unwrap();
        assert!(
            up.starts_with(b"CONNECT a.example:443 HTTP/1.1\r\nx-puddle-origin: container\r\n"),
            "{}",
            String::from_utf8_lossy(&up)
        );
    }

    #[tokio::test]
    async fn a_client_that_closes_before_its_line_ends_sends_what_it_had_unmarked() {
        let (mut client, mut guest) = tokio::io::duplex(64);
        guest.write_all(b"CONNECT a.exa").await.unwrap();
        drop(guest);
        let mut up = Vec::new();
        send_marked(&mut client, &mut up).await.unwrap();
        assert_eq!(up, b"CONNECT a.exa");
    }

    #[tokio::test]
    async fn a_write_that_fails_is_an_error_not_a_hang() {
        // The host's end of the stream is gone: its writes fail.
        let (mut up, gone) = tokio::io::duplex(8);
        drop(gone);
        let (mut client, mut guest) = tokio::io::duplex(64);
        guest
            .write_all(b"CONNECT a.example:443 HTTP/1.1\r\n")
            .await
            .unwrap();
        assert!(send_marked(&mut client, &mut up).await.is_err());
    }

    #[test]
    fn anything_that_is_not_an_http_1_request_line_goes_up_as_it_is() {
        for first in [
            &b""[..],
            b"CONNECT a.example:443 HTTP/1.1",
            b"\x16\x03\x01\x02\x00\n",
            b"SSH-2.0-OpenSSH_9.6\r\n",
            b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n",
            // A plain request goes to its own server, which must not get the header.
            b"GET http://a.example/ HTTP/1.1\r\nHost: a.example\r\n\r\n",
        ] {
            assert_eq!(mark_request(first), first);
        }
    }
}
