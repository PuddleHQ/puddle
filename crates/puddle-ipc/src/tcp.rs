// SPDX-License-Identifier: AGPL-3.0-or-later
//! The accept loop of every TCP listener in Puddle that serves one task per connection.

use std::future::Future;
use std::net::SocketAddr;
use std::time::Duration;

use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinSet;

/// Accepts connections on `listener` until `stop` is ready, and hands each one to `on_conn` with the
/// [`JoinSet`] that owns the connection tasks (so it can spawn into it, and refuse a connection when
/// the set is full). Finished tasks are reaped before every hand-over, so the set's length counts the
/// connections still open. A failed accept is logged under `label` and followed by `pause`, so a
/// descriptor shortage does not spin. Returns the set, with the listener already closed, so a caller
/// that shuts down can wait for the connections; a caller with no `stop` passes
/// `std::future::pending()`.
pub async fn accept_tcp<F>(
    listener: TcpListener,
    pause: Duration,
    label: &'static str,
    stop: impl Future<Output = ()>,
    mut on_conn: F,
) -> JoinSet<()>
where
    F: FnMut(&mut JoinSet<()>, TcpStream, SocketAddr),
{
    let mut connections = JoinSet::new();
    tokio::pin!(stop);
    loop {
        while connections.try_join_next().is_some() {}
        let accepted = tokio::select! {
            () = &mut stop => break,
            accepted = listener.accept() => accepted,
        };
        match accepted {
            Ok((stream, peer)) => {
                // Connections that ended while the loop waited in `accept` must not count.
                while connections.try_join_next().is_some() {}
                on_conn(&mut connections, stream, peer);
            }
            Err(err) => {
                tracing::warn!(error = %err, listener = label, "accept failed");
                tokio::time::sleep(pause).await;
            }
        }
    }
    drop(listener);
    connections
}

#[cfg(test)]
mod tests;
