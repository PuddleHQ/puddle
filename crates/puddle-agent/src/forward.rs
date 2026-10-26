// SPDX-License-Identifier: GPL-3.0-or-later
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
use puddle_agent_proto::kind::{MAX_PREAMBLE, StreamKind, parse_preamble};
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

    /// Serves one stream the host opened, until the connection ends. A stream that is not a
    /// forward request is dropped.
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
            let preamble = read_line_exact(stream, MAX_PREAMBLE).await?;
            if parse_preamble(&preamble) != Ok((StreamKind::Forward, 1)) {
                return Ok(None);
            }
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
async fn answer(stream: &mut StreamHandle, answer: &Answer) -> bool {
    if stream.write_all(&answer.to_line()).await.is_err() || stream.flush().await.is_err() {
        return false;
    }
    if matches!(answer, Answer::Refused(_)) {
        let _ = stream.shutdown().await;
    }
    true
}

/// What the user is told when the connect failed with `failure`.
fn failure_text(failure: io::ErrorKind, port: u16) -> String {
    if failure == io::ErrorKind::ConnectionRefused {
        format!("nothing in the workspace listens on port {port}")
    } else {
        format!("the workspace did not take a connection on port {port} ({failure})")
    }
}

/// Connects to `port` on the guest's loopback, IPv4 then IPv6; the reason is for the user.
async fn connect(port: u16) -> Result<TcpStream, String> {
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
mod tests {
    use futures_util::StreamExt;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio_yamux::{Config, Session};

    use super::*;

    /// Opens a stream from a "host" session, writes `first` on it, and hands the agent's end to
    /// `Forwards::serve` as the agent would. Returns the host's end and what `serve` took.
    async fn serve(
        forwards: Arc<Forwards>,
        first: &[u8],
    ) -> (StreamHandle, tokio::task::JoinHandle<()>) {
        let (a, b) = tokio::io::duplex(1 << 20);
        let mut host = Session::new_server(a, Config::default());
        let mut agent = Session::new_client(b, Config::default());
        let mut control = host.control();
        tokio::spawn(async move { while host.next().await.is_some() {} });
        let mut opened = control.open_stream().await.unwrap();
        opened.write_all(first).await.unwrap();
        let serving = tokio::spawn(async move {
            let inbound = agent.next().await.unwrap().unwrap();
            let task = tokio::spawn(async move { forwards.serve(inbound).await });
            while agent.next().await.is_some() {}
            task.await.unwrap();
        });
        (opened, serving)
    }

    #[tokio::test]
    async fn a_stream_that_is_not_a_forward_request_is_dropped_without_an_answer() {
        for first in [
            &b"GET / HTTP/1.1\r\n\r\n"[..],
            b"\0puddle-forward/2\n80\n",
            b"\0puddle-control/1\n80\n",
            b"\0puddle-forward/1\nnot-a-port\n",
            b"\0puddle-forward/1\n0\n",
        ] {
            let (mut host, _serving) = serve(Forwards::new(vec![]), first).await;
            let mut buf = Vec::new();
            let read =
                tokio::time::timeout(Duration::from_secs(5), host.read_to_end(&mut buf)).await;
            assert!(read.is_ok(), "{first:?}: the stream was left open");
            assert!(buf.is_empty(), "{first:?}: answered {buf:?}");
        }
    }

    #[tokio::test]
    async fn a_stream_closed_before_its_request_is_dropped() {
        let (mut host, _serving) = serve(Forwards::new(vec![]), b"\0puddle-forward/1\n80").await;
        host.shutdown().await.unwrap();
        let mut buf = Vec::new();
        let read = tokio::time::timeout(Duration::from_secs(5), host.read_to_end(&mut buf)).await;
        assert!(read.is_ok(), "the stream was left open");
        assert!(buf.is_empty(), "answered {buf:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn a_stream_that_sends_no_request_in_time_is_dropped() {
        let (mut host, _serving) = serve(Forwards::new(vec![]), b"\0puddle-forward/1\n").await;
        let mut buf = Vec::new();
        let read = tokio::time::timeout(ANSWER_TIMEOUT * 2, host.read_to_end(&mut buf)).await;
        assert!(read.is_ok(), "the stream was left open");
        assert!(buf.is_empty(), "answered {buf:?}");
    }

    #[tokio::test]
    async fn past_the_limit_a_forward_is_refused_with_a_reason() {
        let forwards = Forwards::new(vec![]);
        let held = forwards
            .open
            .acquire_many(u32::try_from(MAX_FORWARDS).unwrap())
            .await
            .unwrap();
        let (mut host, _serving) =
            serve(Arc::clone(&forwards), b"\0puddle-forward/1\n8080\n").await;
        let mut answer = String::new();
        host.read_to_string(&mut answer).await.unwrap();
        assert!(answer.starts_with("refused too many"), "{answer}");
        drop(held);
    }

    #[test]
    fn a_refusal_means_nothing_listens_and_any_other_failure_is_named() {
        assert_eq!(
            failure_text(io::ErrorKind::ConnectionRefused, 80),
            "nothing in the workspace listens on port 80"
        );
        let other = failure_text(io::ErrorKind::TimedOut, 80);
        assert!(
            other.contains("did not take a connection on port 80") && other.contains("timed out"),
            "{other}"
        );
    }
}
