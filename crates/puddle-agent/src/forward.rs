// SPDX-License-Identifier: AGPL-3.0-or-later
//! The guest end of a port forward: the host opens a `forward` stream
//! ([`puddle_agent_proto::forward`]), the agent connects to that port on the guest's loopback and
//! splices the two.
//!
//! The host names a port and nothing else, so a forward can only reach the guest's own loopback
//! (IPv4 first, then IPv6). The agent refuses its own ports (the proxy, the capture listener, the
//! editor server): they are plumbing, and a forward to the proxy would hand the host's user the
//! guest's route out. Streams are bounded in number, and a stream that says nothing is dropped
//! after the protocol's answer timeout (`puddle_agent_proto::forward::ANSWER_TIMEOUT`).

use std::io;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use puddle_agent_proto::forward::{
    ANSWER_TIMEOUT, Answer, MAX_REQUEST_LINE, parse_request, read_line_exact,
};
use puddle_agent_proto::relay::splice;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio::sync::Semaphore;
use tokio_yamux::StreamHandle;

/// How long the connect to the guest's port may take. Loopback answers at once or not at all.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Forwarded connections the agent serves at once. Each costs a descriptor and a task; a
/// test runner or bundler can open hundreds, so the bound is generous and only stops a runaway.
pub const MAX_FORWARDS: usize = 2048;

/// The guest end of the host's port forwards.
#[derive(Debug)]
pub struct Forwards {
    denied: Vec<u16>,
    open: Semaphore,
}

impl Forwards {
    /// Forwards that refuse `denied` ports.
    #[must_use]
    pub fn new(denied: Vec<u16>) -> Arc<Self> {
        Arc::new(Self {
            denied,
            open: Semaphore::new(MAX_FORWARDS),
        })
    }

    /// Serves one `forward` stream the host opened, whose preamble [`crate::host_streams`] has
    /// read, until the connection ends. A stream that sends no valid request is dropped.
    pub async fn serve(&self, mut stream: StreamHandle) {
        let Some(port) = self.read_request(&mut stream).await else {
            return;
        };
        let Ok(_permit) = self.open.try_acquire() else {
            answer(
                &mut stream,
                &Answer::refused("too many forwarded connections in the workspace"),
            )
            .await;
            return;
        };
        if self.denied.contains(&port) {
            answer(
                &mut stream,
                &Answer::refused("that port belongs to Puddle's agent in the workspace"),
            )
            .await;
            return;
        }
        let mut tcp = match connect(port).await {
            Ok(tcp) => tcp,
            Err(reason) => {
                answer(&mut stream, &Answer::refused(&reason)).await;
                return;
            }
        };
        if !answer(&mut stream, &Answer::Ok).await {
            return;
        }
        if let Err(err) = splice(&mut tcp, &mut stream).await {
            // The stream is dropped without a shutdown, which resets it on the host side.
            tracing::debug!(port, error = %err, "forwarded connection ended with an error");
        }
    }

    async fn read_request(&self, stream: &mut StreamHandle) -> Option<u16> {
        let read = async {
            let line = read_line_exact(stream, MAX_REQUEST_LINE).await?;
            Ok::<_, io::Error>(parse_request(&line).ok())
        };
        match tokio::time::timeout(ANSWER_TIMEOUT, read).await {
            Ok(Ok(Some(port))) => Some(port),
            Ok(Ok(None)) => {
                tracing::debug!("a stream from the host that is not a forward request was dropped");
                None
            }
            Ok(Err(err)) => {
                tracing::debug!(error = %err, "a stream from the host ended before its request");
                None
            }
            Err(_) => {
                tracing::debug!("a stream from the host sent no request in time and was dropped");
                None
            }
        }
    }
}

/// Writes `answer`; `false` when the stream is gone. A refusal ends the stream cleanly.
pub(crate) async fn answer(stream: &mut StreamHandle, answer: &Answer) -> bool {
    if stream.write_all(&answer.to_line()).await.is_err() || stream.flush().await.is_err() {
        return false;
    }
    if matches!(answer, Answer::Refused(_)) {
        let _ = stream.shutdown().await;
    }
    true
}

/// What the user is told when the connect failed with `failure`.
pub(crate) fn failure_text(failure: io::ErrorKind, port: u16) -> String {
    if failure == io::ErrorKind::ConnectionRefused {
        format!("nothing in the workspace listens on port {port}")
    } else {
        format!("the workspace did not take a connection on port {port} ({failure})")
    }
}

/// Connects to `port` on the guest's loopback, IPv4 then IPv6; the reason is for the user.
pub(crate) async fn connect(port: u16) -> Result<TcpStream, String> {
    let v4 = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let v6 = SocketAddr::from((Ipv6Addr::LOCALHOST, port));
    // What went wrong, preferring a failure other than "refused": one family refusing is the
    // normal case for a server bound to the other.
    let mut failure = io::ErrorKind::ConnectionRefused;
    for addr in [v4, v6] {
        match tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(addr)).await {
            Ok(Ok(tcp)) => return Ok(tcp),
            Ok(Err(err)) if err.kind() != io::ErrorKind::ConnectionRefused => failure = err.kind(),
            Ok(Err(_)) => {}
            Err(_) => failure = io::ErrorKind::TimedOut,
        }
    }
    Err(failure_text(failure, port))
}

#[cfg(test)]
mod tests;
