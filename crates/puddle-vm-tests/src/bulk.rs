// SPDX-License-Identifier: GPL-3.0-or-later
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
use tokio::sync::Barrier;
use tokio::task::JoinHandle;

/// How much of a `/stall/` body is sent before the silence.
pub const STALL_AFTER_BYTES: usize = 65_536;

const CHUNK: usize = 65_536;
const MAX_HEAD: usize = 16_384;

/// The server. Stops accepting when dropped.
pub struct BulkHost {
    addr: SocketAddr,
    served: Arc<AtomicUsize>,
    task: JoinHandle<()>,
}

impl Drop for BulkHost {
    fn drop(&mut self) {
        self.task.abort();
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
        let barrier = Arc::new(Barrier::new(gate));
        let counted = served.clone();
        let task = tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                tokio::spawn(answer(tcp, barrier.clone(), counted.clone()));
            }
        });
        Ok(Self { addr, served, task })
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

async fn answer(mut tcp: TcpStream, gate: Arc<Barrier>, served: Arc<AtomicUsize>) {
    let mut head = Vec::new();
    let mut chunk = [0u8; 1024];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
        match tcp.read(&mut chunk).await {
            Ok(0) | Err(_) => return,
            Ok(n) => head.extend_from_slice(chunk.get(..n).unwrap_or_default()),
        }
        if head.len() > MAX_HEAD {
            return;
        }
    }
    let Some(ask) = parse_ask(&String::from_utf8_lossy(&head)) else {
        let _ = tcp
            .write_all(
                b"HTTP/1.1 400 Bad Request\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
            )
            .await;
        return;
    };
    let (Ask::Gate(n) | Ask::Stall(n)) = ask;
    let reply = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: application/octet-stream\r\ncontent-length: {n}\r\nconnection: close\r\n\r\n"
    );
    if tcp.write_all(reply.as_bytes()).await.is_err() {
        return;
    }
    let zeros = vec![0u8; CHUNK];
    match ask {
        Ask::Gate(n) => {
            gate.wait().await;
            let mut left = n;
            while left > 0 {
                let k = usize::try_from(left).map_or(CHUNK, |l| l.min(CHUNK));
                if tcp
                    .write_all(zeros.get(..k).unwrap_or_default())
                    .await
                    .is_err()
                {
                    return;
                }
                left -= k as u64;
            }
            let _ = tcp.shutdown().await;
            served.fetch_add(1, Ordering::SeqCst);
        }
        Ask::Stall(n) => {
            let first = usize::try_from(n).map_or(STALL_AFTER_BYTES, |n| n.min(STALL_AFTER_BYTES));
            if tcp
                .write_all(zeros.get(..first).unwrap_or_default())
                .await
                .is_err()
            {
                return;
            }
            // Hold the connection open until the client gives up: read returns when it closes.
            let _ = tcp.read(&mut chunk).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ask_reads_both_paths_and_refuses_the_rest() {
        assert_eq!(
            parse_ask("GET /gate/2097152 HTTP/1.1\r\nhost: x\r\n\r\n"),
            Some(Ask::Gate(2_097_152))
        );
        assert_eq!(
            parse_ask("GET /stall/10 HTTP/1.1\r\n\r\n"),
            Some(Ask::Stall(10))
        );
        for bad in [
            "",
            "POST /gate/1 HTTP/1.1",
            "GET /other/1 HTTP/1.1",
            "GET /gate/x HTTP/1.1",
            "GET /gate HTTP/1.1",
            "GET gate/1 HTTP/1.1",
            "GET",
        ] {
            assert_eq!(parse_ask(bad), None, "{bad:?}");
        }
    }

    async fn get(addr: SocketAddr, path: &str) -> Vec<u8> {
        let mut tcp = TcpStream::connect(addr).await.unwrap();
        tcp.write_all(format!("GET {path} HTTP/1.1\r\nhost: t\r\n\r\n").as_bytes())
            .await
            .unwrap();
        let mut all = Vec::new();
        tcp.read_to_end(&mut all).await.unwrap();
        all
    }

    fn body_of(reply: &[u8]) -> &[u8] {
        let at = reply.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        &reply[at + 4..]
    }

    #[tokio::test]
    async fn a_gate_of_one_serves_the_whole_body_and_counts_it() {
        let host = BulkHost::start(1).await.unwrap();
        let reply = get(host.addr(), "/gate/200000").await;
        assert!(reply.starts_with(b"HTTP/1.1 200 OK\r\n"));
        assert_eq!(body_of(&reply).len(), 200_000);
        assert!(body_of(&reply).iter().all(|b| *b == 0));
        // `served` counts after the shutdown the client has just seen end.
        puddle_test_kit::eventually_async(
            "the served count",
            std::time::Duration::from_secs(5),
            || async {
                if host.served() == 1 {
                    Ok(())
                } else {
                    Err(host.served())
                }
            },
        )
        .await;
    }

    /// Reads from `tcp` until a reply's head is complete and `body` bytes follow it.
    async fn read_reply(tcp: &mut TcpStream, body: usize) -> Vec<u8> {
        let mut got = Vec::new();
        let mut buf = [0u8; 8192];
        while !got.windows(4).any(|w| w == b"\r\n\r\n") || body_of(&got).len() < body {
            let n = tcp.read(&mut buf).await.unwrap();
            assert_ne!(n, 0, "the server closed early");
            got.extend_from_slice(&buf[..n]);
        }
        got
    }

    /// True when `tcp` sends nothing for a short while. Absence cannot be waited for, so this is a
    /// bounded look: a server that wrongly sent would have done so within milliseconds on loopback.
    async fn stays_silent(tcp: &mut TcpStream) -> bool {
        let mut buf = [0u8; 8192];
        tokio::time::timeout(std::time::Duration::from_millis(300), tcp.read(&mut buf))
            .await
            .is_err()
    }

    async fn ask(addr: SocketAddr, path: &str) -> TcpStream {
        let mut tcp = TcpStream::connect(addr).await.unwrap();
        tcp.write_all(format!("GET {path} HTTP/1.1\r\nhost: t\r\n\r\n").as_bytes())
            .await
            .unwrap();
        tcp
    }

    #[tokio::test]
    async fn a_gate_of_two_holds_the_first_body_until_the_second_asks() {
        let host = BulkHost::start(2).await.unwrap();
        let mut first = ask(host.addr(), "/gate/10").await;
        // The head comes at once; the body waits while the first asker is alone at the gate.
        let head = read_reply(&mut first, 0).await;
        assert_eq!(body_of(&head), b"");
        assert!(
            stays_silent(&mut first).await,
            "a body went out before the gate opened"
        );
        let second = get(host.addr(), "/gate/10").await;
        assert_eq!(body_of(&second).len(), 10);
        let mut body = [1u8; 10];
        first.read_exact(&mut body).await.unwrap();
        assert_eq!(body, [0u8; 10]);
    }

    #[tokio::test]
    async fn a_stall_sends_a_head_and_64_kib_then_nothing() {
        let host = BulkHost::start(1).await.unwrap();
        let mut tcp = ask(host.addr(), "/stall/1000000").await;
        let got = read_reply(&mut tcp, STALL_AFTER_BYTES).await;
        assert_eq!(body_of(&got).len(), STALL_AFTER_BYTES);
        assert!(stays_silent(&mut tcp).await, "the stalled stream went on");
    }

    #[tokio::test]
    async fn dropping_the_host_stops_it_listening() {
        let host = BulkHost::start(1).await.unwrap();
        let addr = host.addr();
        drop(host);
        puddle_test_kit::eventually_async(
            "the port refusing connections",
            std::time::Duration::from_secs(5),
            || async { TcpStream::connect(addr).await.is_err() },
        )
        .await;
    }

    /// A request for 5 bytes whose head is exactly `total` bytes long, padded with one header.
    fn request_of(total: usize) -> Vec<u8> {
        let start = "GET /gate/5 HTTP/1.1\r\nx: ";
        let end = "\r\n\r\n";
        let pad = total - start.len() - end.len();
        format!("{start}{}{end}", "a".repeat(pad)).into_bytes()
    }

    async fn send_raw(addr: SocketAddr, request: &[u8]) -> Vec<u8> {
        let mut tcp = TcpStream::connect(addr).await.unwrap();
        tcp.write_all(request).await.unwrap();
        let mut all = Vec::new();
        // The server may close on an oversized head while the rest is still being sent.
        let _ = tcp.read_to_end(&mut all).await;
        all
    }

    #[tokio::test]
    async fn a_head_that_needs_several_reads_is_read_to_its_end() {
        let host = BulkHost::start(1).await.unwrap();
        let reply = send_raw(host.addr(), &request_of(3000)).await;
        assert!(reply.starts_with(b"HTTP/1.1 200 OK\r\n"), "{reply:?}");
        assert_eq!(body_of(&reply).len(), 5);
    }

    #[tokio::test]
    async fn a_head_of_exactly_the_limit_is_served_and_one_byte_more_is_dropped() {
        let host = BulkHost::start(1).await.unwrap();
        let at_limit = send_raw(host.addr(), &request_of(MAX_HEAD)).await;
        assert!(at_limit.starts_with(b"HTTP/1.1 200 OK\r\n"), "{at_limit:?}");
        let over = send_raw(host.addr(), &request_of(MAX_HEAD + 1)).await;
        assert_eq!(over, b"", "an oversized head got an answer");
    }

    #[tokio::test]
    async fn the_served_count_starts_at_zero() {
        assert_eq!(BulkHost::start(1).await.unwrap().served(), 0);
    }

    #[tokio::test]
    async fn a_request_that_is_not_one_of_ours_gets_a_400() {
        let host = BulkHost::start(1).await.unwrap();
        let reply = get(host.addr(), "/elsewhere/1").await;
        assert!(reply.starts_with(b"HTTP/1.1 400 "), "{reply:?}");
    }
}
