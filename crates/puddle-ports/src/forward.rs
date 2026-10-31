// SPDX-License-Identifier: GPL-3.0-or-later
//! One forward: a listener on this computer's loopback whose every connection is carried into the
//! guest, to one port on the guest's loopback, through a stream the agent serves
//! ([`puddle_agent_proto::forward`]). It does not use SSH, so an editor's own forwards and these
//! never count against each other.
//!
//! The listener binds `127.0.0.1` only and is entered in the registry of Puddle's own listeners,
//! which blocks every workspace from reaching it ([`puddle_netpolicy::PuddleEndpoints`]): a guest
//! cannot use another workspace's forward, or its own, to dial back out through the host.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use puddle_agent_proto::forward::GuestLink;
use puddle_agent_proto::relay::splice;
use puddle_netpolicy::{EndpointKind, PuddleEndpoints, Registration};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Semaphore, watch};
use tokio::task::JoinHandle;

/// How long a connection waits for the workspace's agent to be connected. A forward is restored
/// while the workspace starts, and the agent says hello a moment later.
const LINK_WAIT: Duration = Duration::from_secs(5);

/// Connections one forward carries at once, unless [`crate::Ports::with_connection_limit`] says
/// otherwise. A test runner or a bundler can open hundreds through one port; past the limit a
/// connection is reset instead of costing this computer descriptors without end.
pub const MAX_CONNECTIONS: usize = 1024;

/// The way into one workspace's guest, as the agent last offered it. Shared by every forward of the
/// workspace: an agent that restarts gives a new link and the forwards use it from then on.
pub(crate) type LinkCell = Arc<watch::Sender<Option<GuestLink>>>;

/// A new, empty [`LinkCell`].
pub(crate) fn link_cell() -> LinkCell {
    Arc::new(watch::channel(None).0)
}

/// A forward that is listening. Dropping it closes the listener and every connection it carries.
#[derive(Debug)]
pub(crate) struct Forward {
    /// Why the last connection through it failed; cleared by the next one that worked.
    pub(crate) last_error: Arc<Mutex<LastError>>,
    task: JoinHandle<()>,
    _registration: Registration,
}

impl Drop for Forward {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Forward {
    /// Starts carrying `listener`'s connections to `guest_port` through `link`.
    pub(crate) fn start(
        listener: TcpListener,
        guest_port: u16,
        link: &LinkCell,
        endpoints: &PuddleEndpoints,
        max_connections: usize,
    ) -> std::io::Result<Self> {
        let local = listener.local_addr()?;
        let registration = endpoints.register(local, EndpointKind::PinnedForward);
        let last_error = Arc::new(Mutex::new(LastError::default()));
        let task = tokio::spawn(serve(
            listener,
            guest_port,
            link.subscribe(),
            Arc::clone(&last_error),
            max_connections,
        ));
        Ok(Self {
            last_error,
            task,
            _registration: registration,
        })
    }

    /// Closes the listener and every connection it carries, and returns once the listener is gone:
    /// a connection attempted after this fails. Dropping a forward only asks its task to stop, and
    /// the listener can accept a little longer (it did, on Windows).
    pub(crate) async fn close(mut self) {
        self.task.abort();
        // Cancelled is the expected end; a panic in the task was logged by the runtime.
        let _ = (&mut self.task).await;
    }

    /// Why the last connection failed, if it did.
    pub(crate) fn problem(&self) -> Option<String> {
        self.last_error
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .problem
            .clone()
    }
}

async fn serve(
    listener: TcpListener,
    guest_port: u16,
    link: watch::Receiver<Option<GuestLink>>,
    last_error: Arc<Mutex<LastError>>,
    max_connections: usize,
) {
    let open = Arc::new(Semaphore::new(max_connections));
    let mut carried = tokio::task::JoinSet::new();
    let mut ordinal = 0_u64;
    loop {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((client, _)) => {
                    ordinal += 1;
                    let ordinal = ordinal;
                    let Ok(permit) = Arc::clone(&open).try_acquire_owned() else {
                        reset(&client);
                        note(&last_error, ordinal, Some("too many connections through this forward at once".into()));
                        continue;
                    };
                    let (link, last_error) = (link.clone(), Arc::clone(&last_error));
                    carried.spawn(async move {
                        carry(client, guest_port, ordinal, link, &last_error).await;
                        drop(permit);
                    });
                }
                Err(err) => {
                    tracing::warn!(guest_port, error = %err, "a forward could not accept");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            },
            Some(_) = carried.join_next(), if !carried.is_empty() => {}
        }
    }
}

/// Carries one client connection into the guest, or resets it and notes why.
async fn carry(
    mut client: TcpStream,
    guest_port: u16,
    ordinal: u64,
    mut link: watch::Receiver<Option<GuestLink>>,
    last_error: &Mutex<LastError>,
) {
    let link = match tokio::time::timeout(LINK_WAIT, link.wait_for(Option::is_some)).await {
        Ok(Ok(link)) => link.clone(),
        _ => None,
    };
    let Some(link) = link else {
        note(
            last_error,
            ordinal,
            Some("the workspace's agent is not connected".into()),
        );
        reset(&client);
        return;
    };
    let mut stream = match link.open_forward(guest_port).await {
        Ok(stream) => stream,
        Err(err) => {
            note(last_error, ordinal, Some(err.to_string()));
            reset(&client);
            return;
        }
    };
    note(last_error, ordinal, None);
    // A failed copy resets both ends: the client here, and the stream by being dropped.
    if let Err(err) = splice(&mut client, &mut stream).await {
        tracing::debug!(guest_port, error = %err, "a forwarded connection ended with an error");
    }
}

/// Ends a connection with a reset, so the client sees a failure and not an empty answer.
fn reset(client: &TcpStream) {
    if let Err(err) = client.set_zero_linger() {
        tracing::debug!(error = %err, "zero linger on a refused forward connection");
    }
}

/// Why the newest connection that has reported failed, and which connection that was.
#[derive(Debug, Default)]
pub(crate) struct LastError {
    ordinal: u64,
    problem: Option<String>,
}

/// Records how connection number `ordinal` (counted in accept order) ended its set-up. A connection
/// that reports late, after a newer one already did, must not overwrite the newer one's word: a
/// slow success would otherwise wipe the "too many connections" the refused connection after it
/// just noted.
fn note(last_error: &Mutex<LastError>, ordinal: u64, problem: Option<String>) {
    let mut last = last_error.lock().unwrap_or_else(PoisonError::into_inner);
    if ordinal >= last.ordinal {
        *last = LastError { ordinal, problem };
    }
}
