// SPDX-License-Identifier: GPL-3.0-or-later
//! The listener: binds the loopback, enters itself in the registry of Puddle's own listeners and
//! serves every connection with [`crate::proxy`].

use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::{TokioIo, TokioTimer};
use puddle_netpolicy::{EndpointKind, PuddleEndpoints, Registration};
use puddle_types::WorkspaceName;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio::task::{JoinHandle, JoinSet};

use crate::editor::{Guests, Unreachable};
use crate::keys::ServerHalves;
use crate::proxy::{Shared, handle};
use crate::windows::Windows;

/// Client connections served at once. An editor window holds a handful (its page, its sockets, its
/// webviews); past this a connection is dropped instead of costing the computer descriptors.
pub const MAX_CONNECTIONS: usize = 1024;

/// How long a client may take to send a request's head.
const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(30);

/// Pause after a failed accept, so a descriptor shortage does not spin.
const ACCEPT_BACKOFF: Duration = Duration::from_millis(50);

/// The front, listening. Dropping it stops the listener and ends every connection it carries.
#[derive(Debug)]
pub struct Front {
    addr: SocketAddr,
    taken: bool,
    windows: Windows,
    guests: Arc<dyn Guests>,
    halves: Arc<dyn ServerHalves>,
    task: JoinHandle<()>,
    _stop: watch::Sender<bool>,
    _registration: Registration,
}

impl std::fmt::Debug for dyn Guests {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Guests")
    }
}

impl std::fmt::Debug for dyn ServerHalves {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ServerHalves")
    }
}

impl Drop for Front {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// The origin of `workspace`'s editor on a front that listens on `port`, for a caller that knows the
/// port and holds no [`Front`].
#[must_use]
pub fn origin_of(port: u16, workspace: &WorkspaceName) -> String {
    format!("http://{workspace}.localhost:{port}")
}

impl Front {
    /// Binds `127.0.0.1` and starts serving. A workspace's origin includes the port, and a browser
    /// keeps the editor's storage per origin, so the port should stay the same between runs:
    /// `preferred` is tried first, and any free port is taken when it is not available
    /// ([`Front::preferred_port_was_taken`] says so, for the caller to store the new one).
    ///
    /// # Errors
    ///
    /// No loopback port could be bound.
    pub async fn bind(
        preferred: Option<u16>,
        endpoints: &PuddleEndpoints,
        guests: Arc<dyn Guests>,
        halves: Arc<dyn ServerHalves>,
    ) -> io::Result<Self> {
        let (listener, taken) = match preferred {
            Some(port) => match TcpListener::bind((Ipv4Addr::LOCALHOST, port)).await {
                Ok(listener) => (listener, false),
                Err(err) => {
                    tracing::info!(port, error = %err, "the front's usual port is not free; taking another");
                    (TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?, true)
                }
            },
            None => (TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?, false),
        };
        let addr = listener.local_addr()?;
        let registration = endpoints.register(addr, EndpointKind::BrowserFront);
        let windows = Windows::default();
        let (stop_tx, stop) = watch::channel(false);
        let shared = Arc::new(Shared {
            guests: Arc::clone(&guests),
            halves: Arc::clone(&halves),
            windows: windows.clone(),
            port: addr.port(),
            stop,
            sockets: Arc::new(tokio::sync::Semaphore::new(MAX_CONNECTIONS)),
        });
        let task = tokio::spawn(serve(listener, shared));
        Ok(Self {
            addr,
            taken,
            windows,
            guests,
            halves,
            task,
            _stop: stop_tx,
            _registration: registration,
        })
    }

    /// The address the front listens on.
    #[must_use]
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    /// Whether the port asked for in [`Front::bind`] was not free and another was taken.
    #[must_use]
    pub fn preferred_port_was_taken(&self) -> bool {
        self.taken
    }

    /// The origin of `workspace`'s editor: where a window opens it.
    #[must_use]
    pub fn origin(&self, workspace: &WorkspaceName) -> String {
        origin_of(self.addr.port(), workspace)
    }

    /// The editor connections `workspace` has open now (see [`Windows`]).
    #[must_use]
    pub fn connected(&self, workspace: &WorkspaceName) -> usize {
        self.windows.connected(workspace)
    }

    /// The connection counts, for waiting until a workspace has none.
    #[must_use]
    pub fn windows(&self) -> &Windows {
        &self.windows
    }

    /// The editor server's password, for a window to sign in with. It is the one secret the front
    /// hands out, to the caller in this process only.
    ///
    /// # Errors
    ///
    /// The workspace is not running, or its agent has no password to give yet.
    pub async fn password(&self, workspace: &WorkspaceName) -> Result<String, Unreachable> {
        let target = self
            .guests
            .target(workspace)
            .ok_or_else(|| Unreachable("the workspace is not running".into()))?;
        target.editor.password().await
    }

    /// Stops the listener and ends every connection, and returns once the listener is gone.
    pub async fn close(mut self) {
        self.task.abort();
        // Cancelled is the expected end; a panic in the task was logged by the runtime.
        let _ = (&mut self.task).await;
    }
}

async fn serve(listener: TcpListener, shared: Arc<Shared>) {
    let mut connections = JoinSet::new();
    loop {
        while connections.try_join_next().is_some() {}
        let (stream, peer) = match listener.accept().await {
            Ok(accepted) => accepted,
            Err(err) => {
                tracing::warn!(error = %err, "the front could not accept");
                tokio::time::sleep(ACCEPT_BACKOFF).await;
                continue;
            }
        };
        if !peer.ip().is_loopback() || connections.len() >= MAX_CONNECTIONS {
            tracing::warn!(open = connections.len(), "a front connection was refused");
            continue;
        }
        let shared = Arc::clone(&shared);
        let service = service_fn(move |request| handle(Arc::clone(&shared), request));
        let connection = http1::Builder::new()
            .timer(TokioTimer::new())
            .header_read_timeout(HEADER_READ_TIMEOUT)
            .serve_connection(TokioIo::new(stream), service)
            .with_upgrades();
        connections.spawn(async move {
            if let Err(err) = connection.await {
                tracing::debug!(error = %err, "a front connection ended with an error");
            }
        });
    }
}
