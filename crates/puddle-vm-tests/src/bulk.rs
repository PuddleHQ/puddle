// SPDX-License-Identifier: AGPL-3.0-or-later
//! A bulk byte source for the vsock load tests: an HTTP/1.1 server that answers `GET /gate/<n>` with
//! `n` zero bytes once a whole load has connected, and `GET /stall/<n>` with a head and the first
//! 64 KiB of `n` bytes, then nothing until the client leaves. Test code only.
//!
//! The gate is what makes the load simultaneous: no body starts until `gate` connections have asked
//! for one, so every stream of a run is open at the same moment and the test does not depend on how
//! fast the client starts its transfers. A client that never opens `gate` connections sees silence
//! and its stall detector fires, which is the right failure.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Barrier, Notify};

/// How much of a `/stall/` body is sent before the silence.
pub const STALL_AFTER_BYTES: usize = 65_536;

const CHUNK: usize = 65_536;
const MAX_HEAD: usize = 16_384;

/// The server. Stops accepting when dropped (answers in progress finish or end with their client).
pub struct BulkHost {
    addr: SocketAddr,
    served: Arc<AtomicUsize>,
    stop: Arc<Notify>,
}

impl Drop for BulkHost {
    fn drop(&mut self) {
        self.stop.notify_one();
    }
}

/// What a request line asks for.
#[derive(Debug, PartialEq, Eq)]
enum Ask {
    Gate(u64),
    Stall(u64),
}

/// Reads `GET /gate/<n>` or `GET /stall/<n>` from the head of a request.
fn parse_ask(head: &str) -> Option<Ask> {
    let line = head.lines().next()?;
    let mut parts = line.split_whitespace();
    if parts.next()? != "GET" {
        return None;
    }
    let path = parts.next()?;
    let (kind, n) = path.strip_prefix('/')?.split_once('/')?;
    let n = n.parse().ok()?;
    match kind {
        "gate" => Some(Ask::Gate(n)),
        "stall" => Some(Ask::Stall(n)),
        _ => None,
    }
}

impl BulkHost {
    /// Starts the server on a loopback port; `gate` is how many `/gate/` requests must be waiting
    /// before any of them gets its body.
    ///
    /// # Errors
    ///
    /// When no loopback port can be bound.
    pub async fn start(gate: usize) -> std::io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let addr = listener.local_addr()?;
        let served = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(Notify::new());
        tokio::spawn(accept_loop(
            listener,
            Arc::new(Barrier::new(gate)),
            served.clone(),
            stop.clone(),
        ));
        Ok(Self { addr, served, stop })
    }

    /// Where the server listens.
    #[must_use]
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// How many `/gate/` bodies have been sent to the end.
    #[must_use]
    pub fn served(&self) -> usize {
        self.served.load(Ordering::SeqCst)
    }
}

/// Answers every connection on its own task until `stop` is notified, then closes the listener.
async fn accept_loop(
    listener: TcpListener,
    gate: Arc<Barrier>,
    served: Arc<AtomicUsize>,
    stop: Arc<Notify>,
) {
    loop {
        tokio::select! {
            () = stop.notified() => return,
            Ok((tcp, _)) = listener.accept() => {
                let (gate, served) = (gate.clone(), served.clone());
                tokio::spawn(async move {
                    // A client that leaves mid-answer is not the server's problem.
                    let _ = answer(tcp, &gate, &served).await;
                });
            }
        }
    }
}

async fn answer(mut tcp: TcpStream, gate: &Barrier, served: &AtomicUsize) -> std::io::Result<()> {
    let mut head = Vec::new();
    let mut chunk = [0u8; 1024];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
        let n = tcp.read(&mut chunk).await?;
        if n == 0 || head.len() + n > MAX_HEAD {
            return Ok(());
        }
        head.extend_from_slice(chunk.get(..n).unwrap_or_default());
    }
    let Some(ask) = parse_ask(&String::from_utf8_lossy(&head)) else {
        return tcp
            .write_all(
                b"HTTP/1.1 400 Bad Request\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
            )
            .await;
    };
    let (Ask::Gate(n) | Ask::Stall(n)) = ask;
    let reply = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/octet-stream\r\ncontent-length: {n}\r\nconnection: close\r\n\r\n"
    );
    tcp.write_all(reply.as_bytes()).await?;
    let zeros = vec![0u8; CHUNK];
    match ask {
        Ask::Gate(n) => {
            gate.wait().await;
            let mut left = n;
            while left > 0 {
                let k = usize::try_from(left).map_or(CHUNK, |l| l.min(CHUNK));
                tcp.write_all(zeros.get(..k).unwrap_or_default()).await?;
                left -= k as u64;
            }
            tcp.shutdown().await?;
            served.fetch_add(1, Ordering::SeqCst);
        }
        Ask::Stall(n) => {
            let first = usize::try_from(n).map_or(STALL_AFTER_BYTES, |n| n.min(STALL_AFTER_BYTES));
            tcp.write_all(zeros.get(..first).unwrap_or_default())
                .await?;
            // Hold the connection open until the client gives up: read returns when it closes.
            let _closed = tcp.read(&mut chunk).await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
