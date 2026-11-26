// SPDX-License-Identifier: AGPL-3.0-or-later
//! The proxy listener inside the guest: every accepted connection becomes one yamux stream to
//! the host, spliced so an abort stays an abort. The agent doesn't parse the request;
//! the host proxy does, except for two things in a `CONNECT`: a request from a container (see
//! `crate::origin`) gets [`ORIGIN_HEADER`] after its first line, so the proxy does not decrypt it,
//! and one from a process the agent can name (see `crate::program`) gets [`PROGRAM_HEADER`]. A
//! `x-puddle-program` header the client sent itself is removed first: only the agent names a program.

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use puddle_agent_proto::origin::{CONTAINER, ORIGIN_HEADER};
use puddle_agent_proto::program::{PROGRAM_HEADER, Program};
use puddle_agent_proto::relay::splice;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpSocket, TcpStream};

use crate::upstream::Upstream;

/// Pause after a failed accept (e.g. out of file descriptors) so the loop doesn't spin.
const ACCEPT_PAUSE: Duration = Duration::from_millis(50);

/// Connections the kernel may hold for the accept loop before it starts dropping handshakes (the
/// kernel clamps this to `net.core.somaxconn`, 4096 on current kernels). A build tool opens
/// thousands at once, and tokio's own default of 128 overflows: the dropped handshakes come back
/// as retransmits seconds later or, rarely, as a reset.
pub const LISTEN_BACKLOG: u32 = 4096;

/// How long a client gets to send its request head before the connection goes up unmarked.
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
    puddle_ipc::accept_tcp(
        listener,
        ACCEPT_PAUSE,
        "guest connections",
        std::future::pending(),
        |conns, client, _| {
            conns.spawn(connection(client, Arc::clone(&upstream)));
        },
    )
    .await;
}

/// What a client sent first: up to and including its request head (the blank line after the
/// headers), plus whatever came in the same reads. Ends early (with what there is) on a closed
/// connection, a pause over [`REQUEST_LINE_WAIT`] or a head over [`REQUEST_LINE_MAX`].
async fn request_start<R: AsyncRead + Unpin>(client: &mut R) -> Vec<u8> {
    let mut seen = Vec::new();
    let mut chunk = [0u8; 1024];
    let deadline = tokio::time::Instant::now() + REQUEST_LINE_WAIT;
    while !head_complete(&seen) && !not_a_connect(&seen) && seen.len() < REQUEST_LINE_MAX {
        match tokio::time::timeout_at(deadline, client.read(&mut chunk)).await {
            Ok(Ok(n)) if n > 0 => seen.extend_from_slice(chunk.get(..n).unwrap_or_default()),
            _ => break,
        }
    }
    seen
}

/// Whether `seen` holds a whole request head: its blank line (a bare line feed ends a line too).
fn head_complete(seen: &[u8]) -> bool {
    seen.windows(2).any(|w| w == b"\n\n") || seen.windows(3).any(|w| w == b"\n\r\n")
}

/// Whether `seen` already shows a request that [`mark_request`] leaves alone, so waiting for the
/// rest of its head would only delay it: a first line that is not a `CONNECT`.
fn not_a_connect(seen: &[u8]) -> bool {
    let Some(end) = seen.iter().position(|b| *b == b'\n') else {
        return !b"CONNECT ".starts_with(seen.get(..8).unwrap_or(seen));
    };
    !seen.get(..end).unwrap_or_default().starts_with(b"CONNECT ")
}

/// Whether the header line `line` is [`PROGRAM_HEADER`], whatever its case.
fn is_program_header(line: &[u8]) -> bool {
    line.split(|b| *b == b':').next().is_some_and(|name| {
        name.trim_ascii()
            .eq_ignore_ascii_case(PROGRAM_HEADER.as_bytes())
    })
}

/// `first` with the origin header (when `container`) and the program header (when `program` is
/// known) after its request line, and any program header the client sent removed. Only a
/// `CONNECT` is changed: the proxy decides about decrypting and about the label at the tunnel, and
/// a plain request is forwarded to its server, which must not see the headers. Anything else is
/// returned as it is.
fn mark_request(first: &[u8], container: bool, program: Option<&Program>) -> Vec<u8> {
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
    if container {
        out.extend_from_slice(format!("{ORIGIN_HEADER}: {CONTAINER}\r\n").as_bytes());
    }
    if let Some(program) = program {
        out.extend_from_slice(format!("{PROGRAM_HEADER}: {program}\r\n").as_bytes());
    }
    // Drop the client's own program header lines, but only from the head: the body is not ours.
    let mut in_head = true;
    for header in rest.split_inclusive(|b| *b == b'\n') {
        if in_head && header.trim_ascii().is_empty() {
            in_head = false;
        }
        if !(in_head && is_program_header(header)) {
            out.extend_from_slice(header);
        }
    }
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

/// Reads a client's request start and sends it to the host with the origin and program headers
/// added.
async fn send_marked<R, W>(
    client: &mut R,
    stream: &mut W,
    container: bool,
    program: Option<&Program>,
) -> io::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let first = request_start(client).await;
    stream
        .write_all(&mark_request(&first, container, program))
        .await
        .inspect_err(|err| tracing::debug!(error = %err, "writing a client's request failed"))
}

/// The program of `client` (see [`crate::program`]; only Linux guests resolve one). A container's
/// client has a source address that is not the guest's own, so it has none.
#[cfg(target_os = "linux")]
async fn named(client: &TcpStream, container: bool) -> Option<Program> {
    if container {
        return None;
    }
    crate::program::of_client(client).await
}

/// Only Linux guests resolve a program; elsewhere the label is absent.
#[cfg(not(target_os = "linux"))]
#[expect(
    clippy::unused_async,
    reason = "keeps the signature of the Linux resolver, which awaits"
)]
async fn named(_client: &TcpStream, _container: bool) -> Option<Program> {
    None
}

async fn connection(mut client: TcpStream, upstream: Arc<Upstream>) {
    // Responses reach the client in many small writes; Nagle would hold each tail back for an ACK.
    if let Err(err) = client.set_nodelay(true) {
        tracing::debug!(error = %err, "nodelay");
    }
    let mut stream = match upstream.open().await {
        Ok(stream) => stream,
        Err(err) => {
            tracing::warn!(error = %err, "no stream to the host; connection reset");
            // A reset, so the client sees a failure rather than an empty response.
            let _ = client.set_zero_linger();
            return;
        }
    };
    let container = in_container(&client);
    let program = named(&client, container).await;
    tracing::debug!(
        container,
        program = program.is_some(),
        "guest connection opened a stream to the host"
    );
    if send_marked(&mut client, &mut stream, container, program.as_ref())
        .await
        .is_err()
    {
        return;
    }
    match splice(&mut client, &mut stream).await {
        Ok((to_host, to_guest)) => tracing::trace!(to_host, to_guest, "guest connection ended"),
        // The stream is dropped without a shutdown below, which resets it on the host side.
        Err(err) => tracing::debug!(error = %err, "guest connection ended with an error"),
    }
}

#[cfg(test)]
mod tests;
