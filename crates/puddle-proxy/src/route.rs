// SPDX-License-Identifier: AGPL-3.0-or-later
//! The host listener on one workspace's route: accept agent connections and serve
//! each as a yamux session whose proxied streams go to that workspace's [`WorkspaceHandler`].
//!
//! The endpoint is the identity: every connection on it belongs to the workspace the route
//! was created for, whatever the guest says.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use puddle_agent_proto::host::{SessionError, SessionId, serve_session};
use puddle_ipc::{Endpoint, IpcError, Listener};
use puddle_types::WorkspaceId;
use tokio::sync::oneshot;
use tokio::task::{self, JoinHandle, JoinSet};
use tracing::Instrument;

use crate::proxy::{Proxy, WorkspaceHandler};

/// Pause after an accept error, so a broken listener can't spin a core.
const ACCEPT_BACKOFF: Duration = Duration::from_millis(100);

/// A route being served. Dropping it (or [`Route::shutdown`]) stops accepting and ends every
/// session on it; open connections are reset. A dropped route stops through its `stop` sender
/// closing, the same path as a shutdown, so it needs no `Drop` of its own.
#[derive(Debug)]
pub struct Route {
    endpoint: Endpoint,
    workspace: WorkspaceId,
    task: JoinHandle<()>,
    /// Asks the accept loop to close its listener and end; dropping it does the same.
    stop: Option<oneshot::Sender<()>>,
}

impl Route {
    /// The endpoint the workspace's vsock route points at.
    #[must_use]
    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// The workspace this route serves.
    #[must_use]
    pub fn workspace(&self) -> &WorkspaceId {
        &self.workspace
    }

    /// Stops the route and waits until its accept loop and sessions are gone and its listener
    /// is closed, so nothing accepts on the endpoint any more (on Windows too, where a pipe
    /// instance keeps listening until it is closed explicitly).
    pub async fn shutdown(mut self) {
        if let Some(stop) = self.stop.take() {
            // A send error means the loop already ended.
            let _ = stop.send(());
        }
        if let Err(err) = (&mut self.task).await
            && err.is_panic()
        {
            tracing::error!(workspace = %self.workspace, "route task panicked");
        }
    }
}

impl Proxy {
    /// Serves `listener` (the route of `workspace`) until the returned [`Route`] is dropped or shut
    /// down. Must be called inside a tokio runtime.
    #[must_use]
    pub fn serve_route(self: &Arc<Self>, listener: Listener, workspace: WorkspaceId) -> Route {
        let endpoint = listener.endpoint().clone();
        let handler = Arc::new(self.handler(workspace.clone()));
        let span = tracing::info_span!("route", workspace = %workspace);
        let (stop, stopped) = oneshot::channel();
        let task = tokio::spawn(
            accept_loop(Arc::clone(self), listener, handler, stopped).instrument(span),
        );
        Route {
            endpoint,
            workspace,
            task,
            stop: Some(stop),
        }
    }
}

/// Cuts off every session still running and tells the guest watcher of each one's end, as
/// `session_ended` promises "however it ended".
async fn end_sessions(
    proxy: &Proxy,
    workspace: &WorkspaceId,
    sessions: &mut JoinSet<Result<(), SessionError>>,
    ids: &mut HashMap<task::Id, SessionId>,
) {
    sessions.shutdown().await;
    for (_, session) in ids.drain() {
        proxy.session_ended(workspace, session);
    }
}

async fn accept_loop(
    proxy: Arc<Proxy>,
    mut listener: Listener,
    handler: Arc<WorkspaceHandler>,
    mut stopped: oneshot::Receiver<()>,
) {
    let max = proxy.config.max_sessions_per_route;
    let mut sessions = JoinSet::new();
    // The session each running task serves.
    let mut ids = HashMap::new();
    loop {
        tokio::select! {
            _ = &mut stopped => {
                end_sessions(&proxy, handler.workspace(), &mut sessions, &mut ids).await;
                listener.close().await;
                return;
            }
            accepted = listener.accept() => match accepted {
                Ok(conn) if sessions.len() >= max => {
                    tracing::warn!(limit = max, "agent connection refused: route over its session limit");
                    drop(conn);
                }
                Ok(conn) => {
                    let workspace = handler.workspace().clone();
                    let sink = Arc::clone(&proxy.sink);
                    let config = proxy.config.session;
                    let handler = Arc::clone(&handler);
                    let session = SessionId::next();
                    let running = sessions.spawn(async move {
                        serve_session(conn, workspace, session, sink, handler, config).await
                    });
                    ids.insert(running.id(), session);
                }
                Err(IpcError::Closed { .. }) => {
                    tracing::info!("route closed");
                    end_sessions(&proxy, handler.workspace(), &mut sessions, &mut ids).await;
                    return;
                }
                Err(err) => {
                    tracing::warn!(error = %err, "accept failed");
                    tokio::time::sleep(ACCEPT_BACKOFF).await;
                }
            },
            Some(done) = sessions.join_next_with_id(), if !sessions.is_empty() => {
                let task = match &done {
                    Ok((task, _)) => *task,
                    Err(err) => err.id(),
                };
                if let Some(session) = ids.remove(&task) {
                    proxy.session_ended(handler.workspace(), session);
                }
                match done {
                Ok((_, Ok(()))) => tracing::debug!("agent session ended"),
                Ok((_, Err(err))) => tracing::info!(error = %err, "agent session failed"),
                Err(err) if err.is_panic() => tracing::error!("agent session panicked"),
                Err(_) => {}
                }
            }
        }
    }
}
