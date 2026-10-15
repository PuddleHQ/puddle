// SPDX-License-Identifier: GPL-3.0-or-later
//! The proxy listener inside the guest: every accepted connection becomes one yamux stream to
//! the host, spliced so an abort stays an abort. The agent doesn't parse the request;
//! the host proxy does.

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use puddle_agent_proto::relay::splice;
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
    if let Err(err) = splice(&mut client, &mut stream).await {
        // The stream is dropped without a shutdown below, which resets it on the host side.
        tracing::debug!(error = %err, "guest connection ended with an error");
    }
}
