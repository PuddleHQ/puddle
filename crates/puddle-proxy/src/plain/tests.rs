// SPDX-License-Identifier: AGPL-3.0-or-later
//! Plain HTTP on a guest connection whose stream fails at one exact point. The guest side is
//! scripted with [`tokio_test::io::Builder`]; the upstream is a loopback server that answers every
//! request. The same connections through a real agent are in `tests/integration`.

use std::io;
use std::net::{IpAddr, Ipv4Addr};
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll};

use puddle_test_kit::capture_logs;
use puddle_types::{ConnectionOutcome, Host, NullSink, WorkspaceId};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::TcpListener;
use tokio_test::io::{Builder, Mock};

use super::*;
use crate::counted::Counted;
use crate::proxy::read_request;
use crate::testing::{AnyAddress, CollectingConnectionLog, StaticPolicy, StaticResolver};

const HOST: &str = "up.test";

struct Rig {
    handler: WorkspaceHandler,
    log: Arc<CollectingConnectionLog>,
    port: u16,
}

/// A handler that allows `up.test`, a loopback server behind that name that answers each request
/// with a two-byte body and keeps the connection, and the audit log it writes to.
async fn rig() -> Rig {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut conn, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut seen = Vec::new();
                let mut chunk = [0_u8; 1024];
                loop {
                    let Ok(n) = conn.read(&mut chunk).await else {
                        return;
                    };
                    if n == 0 {
                        return;
                    }
                    seen.extend_from_slice(&chunk[..n]);
                    while let Some(end) = seen.windows(4).position(|w| w == b"\r\n\r\n") {
                        seen.drain(..end + 4);
                        let answer = b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok";
                        if conn.write_all(answer).await.is_err() {
                            return;
                        }
                    }
                }
            });
        }
    });
    let policy = Arc::new(StaticPolicy::new());
    policy.allow(&Host::parse_normalised(HOST).unwrap());
    let log = Arc::new(CollectingConnectionLog::new());
    let resolver = StaticResolver::new().with(HOST, &[IpAddr::V4(Ipv4Addr::LOCALHOST)]);
    let proxy = Arc::new(
        Proxy::new(policy, Arc::new(NullSink))
            .with_connection_log(log.clone())
            .with_resolver(Arc::new(resolver))
            .with_address_check(Arc::new(AnyAddress)),
    );
    Rig {
        handler: proxy.handler(WorkspaceId::new("box").unwrap()),
        log,
        port,
    }
}

/// Serves `stream` as the proxy serves a guest connection that starts with plain HTTP.
async fn serve_guest<S>(handler: &WorkspaceHandler, stream: S)
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    let (stream, counts) = Counted::new(stream);
    let mut reader = BufReader::new(stream);
    let head = read_request(&mut reader, handler.proxy.config.head_timeout)
        .await
        .unwrap();
    let (target, path, _) = parse_request(&head).unwrap();
    let first = Incoming::new(head, target, path.unwrap()).unwrap();
    serve(handler, reader, &counts, first).await;
}

/// The guest's connection: reads are scripted, what is written is kept, and closing the write side
/// fails, as it does on a transport that is already reset.
struct ResetOnClose {
    script: Mock,
    written: Arc<Mutex<Vec<u8>>>,
}

impl AsyncRead for ResetOnClose {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.script).poll_read(cx, buf)
    }
}

impl AsyncWrite for ResetOnClose {
    fn poll_write(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.written
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend_from_slice(buf);
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Err(io::Error::from(io::ErrorKind::ConnectionReset)))
    }
}

fn request(port: u16, extra: &str) -> String {
    format!("POST http://{HOST}:{port}/ HTTP/1.1\r\nHost: {HOST}:{port}\r\n{extra}\r\n")
}

fn reset() -> io::Error {
    io::Error::from(io::ErrorKind::ConnectionReset)
}

#[tokio::test]
async fn a_guest_that_fails_at_the_100_continue_gets_no_request_upstream_and_is_recorded() {
    let (logs, _guard) = capture_logs();
    let rig = rig().await;
    let guest = Builder::new()
        .read(request(rig.port, "Expect: 100-continue\r\nContent-Length: 5\r\n").as_bytes())
        .write_error(reset())
        .build();
    serve_guest(&rig.handler, guest).await;
    assert!(
        logs.text().contains("guest went away before its body"),
        "{}",
        logs.text()
    );
    let events = rig.log.wait_for(1, std::time::Duration::from_secs(5)).await;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].outcome, Some(ConnectionOutcome::Connected));
}

#[tokio::test]
async fn a_guest_whose_close_fails_after_a_closing_answer_still_got_the_whole_answer() {
    let (logs, _guard) = capture_logs();
    let rig = rig().await;
    let written = Arc::new(Mutex::new(Vec::new()));
    let guest = ResetOnClose {
        script: Builder::new()
            .read(request(rig.port, "Connection: close\r\nContent-Length: 0\r\n").as_bytes())
            .build(),
        written: Arc::clone(&written),
    };
    serve_guest(&rig.handler, guest).await;
    let answer = String::from_utf8(written.lock().unwrap().clone()).unwrap();
    assert!(answer.starts_with("HTTP/1.1 200 OK"), "{answer}");
    assert!(answer.ends_with("ok"), "{answer}");
    assert!(
        logs.text().contains("guest went away before the close"),
        "{}",
        logs.text()
    );
}

#[tokio::test]
async fn a_guest_whose_close_fails_after_it_stops_sending_requests_is_dropped() {
    let (logs, _guard) = capture_logs();
    let rig = rig().await;
    let written = Arc::new(Mutex::new(Vec::new()));
    // One kept-alive request, then the guest ends its side.
    let guest = ResetOnClose {
        script: Builder::new()
            .read(request(rig.port, "Content-Length: 0\r\n").as_bytes())
            .build(),
        written: Arc::clone(&written),
    };
    serve_guest(&rig.handler, guest).await;
    let answer = String::from_utf8(written.lock().unwrap().clone()).unwrap();
    assert!(answer.ends_with("ok"), "{answer}");
    assert!(
        logs.text().contains("guest went away before the close"),
        "{}",
        logs.text()
    );
}
