// SPDX-License-Identifier: GPL-3.0-or-later
//! The per-workspace and per-connection limits of terminated HTTP/2: the stream budget, sockets of
//! the HTTP/1.1 pool, streams that are never polled, a peer that ignores the shutdown, a stream
//! queued behind the server's own limit, a dead server asked by many streams, the window of the
//! guest leg and the idle limit of an agreed WebSocket. Each test says which mechanism it proves.
mod terminate_support;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use bytes::Bytes;
use puddle_proxy::ProxyConfig;
use puddle_types::{ConnectionEvent, ConnectionReason};
use terminate_support::h2_rig::{
    H2Server, Handler, Script, WsServer, ack_server_settings, frame, frames_until_end, full,
    get_block, open_body, raw, reply,
};
use terminate_support::{FakeServer, Flaw, Pki, Reply, RigBuilder};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn ok() -> Script {
    Arc::new(|_| reply(200, "fine"))
}

/// Answers `/hold` never (no head at all) and anything else at once, counting the `/hold`
/// requests that arrived.
fn holding(held: &Arc<AtomicUsize>) -> Handler {
    let held = Arc::clone(held);
    Arc::new(move |request, _| {
        let held = Arc::clone(&held);
        Box::pin(async move {
            if request.uri().path() == "/hold" {
                held.fetch_add(1, Ordering::SeqCst);
                std::future::pending::<()>().await;
            }
            reply(200, "fine")
        })
    })
}

/// As [`holding`], but `/hold` gets its head at once and a body that never ends: the stream stays
/// open at the server without any wait for a head running at the proxy.
fn streaming_forever(held: &Arc<AtomicUsize>) -> Handler {
    let held = Arc::clone(held);
    Arc::new(move |request, _| {
        let held = Arc::clone(&held);
        Box::pin(async move {
            if request.uri().path() == "/hold" {
                held.fetch_add(1, Ordering::SeqCst);
                let mut answer = reply(200, "");
                *answer.body_mut() = open_body_reply();
                return answer;
            }
            reply(200, "fine")
        })
    })
}

fn open_body_reply()
-> http_body_util::combinators::UnsyncBoxBody<Bytes, terminate_support::h2_rig::BoxError> {
    use http_body_util::BodyExt as _;
    http_body_util::StreamBody::new(futures_util::stream::pending::<
        Result<http_body::Frame<Bytes>, terminate_support::h2_rig::BoxError>,
    >())
    .boxed_unsync()
}

async fn until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < deadline, "gave up waiting for {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn reasons(events: &[ConnectionEvent]) -> Vec<ConnectionReason> {
    events.iter().map(|event| event.reason).collect()
}

// ---------------------------------------------------------------------------------------------
// The workspace's budgets

#[tokio::test]
async fn a_stream_over_the_workspace_budget_is_refused_and_a_finished_one_gives_its_place_back() {
    let pki = Pki::new();
    let held = Arc::new(AtomicUsize::new(0));
    let server = H2Server::streaming(&pki, "bound.test", holding(&held)).await;
    let config = ProxyConfig::default().with_max_h2_streams_per_workspace(2);
    let rig = RigBuilder::new(&pki)
        .name("bound.test", server.addr)
        .config(config)
        .build();
    let mut guest = rig.guest().await;
    let mut client = guest.h2("bound.test:443", &[b"h2"]).await;
    let mut holders = Vec::new();
    for _ in 0..2 {
        let mut sender = client.sender.clone();
        holders.push(tokio::spawn(async move {
            let request = http::Request::builder()
                .uri("https://bound.test/hold")
                .body(full(Bytes::new()))
                .unwrap();
            let _ = sender.send_request(request).await;
        }));
    }
    until("both streams to reach the server", || {
        held.load(Ordering::SeqCst) == 2
    })
    .await;
    let refused = client.get("bound.test", "/other").await;
    assert_eq!(refused.status, 503);
    assert_eq!(refused.header("x-puddle-blocked"), Some("stream_limit"));
    assert_eq!(refused.header("retry-after"), Some("1"));
    assert!(
        refused.text().contains("too many open requests"),
        "{refused:?}"
    );
    // Giving up on one of the two streams frees its place.
    holders[0].abort();
    let mut status = 0;
    for _ in 0..100 {
        status = client.get("bound.test", "/other").await.status;
        if status == 200 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(status, 200, "the place came back");
    for holder in &holders {
        holder.abort();
    }
    client.close().await;
    let events = rig.events(1).await;
    assert_eq!(
        reasons(&events),
        [ConnectionReason::Refused("stream_limit")]
    );
}

#[tokio::test]
async fn sockets_of_the_http11_pool_count_against_the_workspace_connection_limit() {
    let pki = Pki::new();
    let server = FakeServer::tls(
        pki.server_config("bound.test", Flaw::None),
        Arc::new(|request| {
            if request.target == "/slow" {
                Reply::ok("slow").after(Duration::from_secs(4))
            } else {
                Reply::ok("quick")
            }
        }),
    )
    .await;
    // The guest connection takes one place; the limit leaves two for the pool's sockets.
    let config = ProxyConfig::default().with_max_streams_per_workspace(3);
    let rig = RigBuilder::new(&pki)
        .name("bound.test", server.addr)
        .config(config)
        .build();
    let mut guest = rig.guest().await;
    let mut client = guest.h2("bound.test:443", &[b"h2"]).await;
    let mut slow = Vec::new();
    for _ in 0..3 {
        let mut sender = client.sender.clone();
        slow.push(tokio::spawn(async move {
            let request = http::Request::builder()
                .uri("https://bound.test/slow")
                .body(full(Bytes::new()))
                .unwrap();
            terminate_support::h2_rig::collect(sender.send_request(request).await.unwrap())
                .await
                .status
        }));
        // One at a time, so each finds the others' sockets busy and wants one of its own.
        let wanted = slow.len();
        until("the server to see the request", || {
            server.recorded().len() == wanted
        })
        .await;
    }
    assert_eq!(server.accepted(), 3, "the first socket and two more");
    let refused = client.get("bound.test", "/quick").await;
    assert_eq!(refused.status, 503, "{refused:?}");
    assert_eq!(refused.header("x-puddle-blocked"), Some("connection_limit"));
    assert_eq!(server.accepted(), 3, "no fourth socket was opened");
    for task in slow {
        assert_eq!(task.await.unwrap(), 200);
    }
    // The sockets are idle now, and still count: a request uses one of them.
    assert_eq!(client.get("bound.test", "/quick").await.status, 200);
    assert_eq!(server.accepted(), 3);
    client.close().await;
    let events = rig.events(1).await;
    assert_eq!(
        reasons(&events),
        [ConnectionReason::Refused("connection_limit")]
    );
}

// ---------------------------------------------------------------------------------------------
// Limits on time

/// A guest that grants its streams no window never lets the proxy poll the response body, so no
/// timer inside the body can fire. The connection's own sweep ends it.
#[tokio::test]
async fn a_stream_that_is_never_polled_ends_its_connection_and_the_audit_says_why() {
    let pki = Pki::new();
    let big = Bytes::from(vec![7_u8; 200 * 1024]);
    let server = H2Server::recording(
        &pki,
        "bound.test",
        Arc::new(move |_| {
            let mut answer = reply(200, "");
            *answer.body_mut() = full(big.clone());
            answer
        }),
    )
    .await;
    let config = ProxyConfig::default().with_idle_limits(
        Duration::from_secs(2),
        Duration::from_secs(60),
        Duration::from_secs(1),
    );
    let rig = RigBuilder::new(&pki)
        .name("bound.test", server.addr)
        .config(config)
        .build();
    let mut guest = rig.guest().await;
    let mut tls = raw(&mut guest).await;
    // SETTINGS: INITIAL_WINDOW_SIZE (4) = 0, so no stream is ever allowed a byte of DATA.
    let mut settings = vec![0, 4];
    settings.extend_from_slice(&0_u32.to_be_bytes());
    tls.write_all(&frame(4, 0, 0, &settings)).await.unwrap();
    tls.write_all(&frame(1, 0x4 | 0x1, 1, &get_block(b"/big")))
        .await
        .unwrap();
    let started = Instant::now();
    let (seen, closed) = frames_until_end(&mut tls, Duration::from_secs(40)).await;
    assert!(closed, "the connection was never ended: {seen:?}");
    assert!(
        seen.iter()
            .any(|(kind, stream, _)| *kind == 1 && *stream == 1),
        "the response head went out: {seen:?}"
    );
    assert!(
        !seen.iter().any(|(kind, ..)| *kind == 0),
        "no DATA without window: {seen:?}"
    );
    assert!(
        started.elapsed() >= Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
    let logs = terminate_support::captured_logs();
    assert!(
        logs.contains("guest HTTP/2 connection dropped") && logs.contains("stream_idle"),
        "the sweep did not end it"
    );
    let events = rig.events(1).await;
    assert_eq!(
        reasons(&events),
        [ConnectionReason::LimitEnded("stream_idle")]
    );
    assert_eq!(events[0].reason.to_string(), "limit:stream_idle");
}

/// A peer that never answers the shutdown's PING keeps a connection that is done.
#[tokio::test]
async fn a_peer_that_ignores_the_shutdown_is_dropped_after_the_grace() {
    let pki = Pki::new();
    let server = H2Server::recording(&pki, "bound.test", ok()).await;
    let config = ProxyConfig::default()
        .with_terminate_timeouts(
            Duration::from_secs(10),
            Duration::from_secs(1),
            Duration::from_secs(10),
            Duration::from_secs(10),
        )
        .with_idle_limits(
            Duration::from_secs(60),
            Duration::from_secs(60),
            Duration::from_secs(1),
        );
    let rig = RigBuilder::new(&pki)
        .name("bound.test", server.addr)
        .config(config)
        .build();
    let mut guest = rig.guest().await;
    let mut tls = raw(&mut guest).await;
    let started = Instant::now();
    // Acknowledges SETTINGS and nothing else: the PING that follows the GOAWAY stays unanswered.
    let (seen, closed) = frames_until_end(&mut tls, Duration::from_secs(30)).await;
    assert!(closed, "{seen:?}");
    assert!(
        seen.iter().any(|(kind, ..)| *kind == 7),
        "the shutdown was announced first: {seen:?}"
    );
    assert!(
        started.elapsed() >= Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
    let events = rig.events(1).await;
    assert_eq!(
        reasons(&events),
        [ConnectionReason::LimitEnded("shutdown_ignored")]
    );
}

/// A stream queued behind the server's own concurrent-stream limit has not started, so the wait
/// for the answer's head (which counts from the end of the request) never began.
#[tokio::test]
async fn a_stream_queued_behind_the_servers_stream_limit_is_answered_after_the_head_timeout() {
    let pki = Pki::new();
    let held = Arc::new(AtomicUsize::new(0));
    let server = H2Server::streaming_limited(&pki, "bound.test", streaming_forever(&held), 1).await;
    let config = ProxyConfig::default().with_terminate_timeouts(
        Duration::from_secs(10),
        Duration::from_secs(60),
        Duration::from_secs(1),
        Duration::from_secs(10),
    );
    let rig = RigBuilder::new(&pki)
        .name("bound.test", server.addr)
        .config(config)
        .build();
    let mut guest = rig.guest().await;
    let mut client = guest.h2("bound.test:443", &[b"h2"]).await;
    let mut first = client.sender.clone();
    let _held = tokio::spawn(async move {
        let request = http::Request::builder()
            .uri("https://bound.test/hold")
            .body(full(Bytes::new()))
            .unwrap();
        // The answer is kept: dropping it would reset the stream and free the server's slot.
        let _answer = first.send_request(request).await;
        std::future::pending::<()>().await;
    });
    until("the first stream to hold the server's only slot", || {
        held.load(Ordering::SeqCst) == 1
    })
    .await;
    // The second stream has a body that is still being sent, so only its start can be waited for.
    let asked = Instant::now();
    let got = tokio::time::timeout(
        Duration::from_secs(15),
        client.request("POST", "bound.test", "/queued", &[], open_body()),
    )
    .await
    .expect("the queued stream was never answered");
    assert_eq!(got.status, 504);
    assert!(
        got.text().contains("did not take up the request within 1s"),
        "{got:?}"
    );
    assert!(
        asked.elapsed() < Duration::from_secs(8),
        "{:?}",
        asked.elapsed()
    );
}

/// A server that takes the connection and never speaks TLS, asked by five streams at once: one
/// attempt answers all of them, instead of five attempts one after the other.
#[tokio::test]
async fn streams_asking_for_a_dead_server_share_one_attempt() {
    let pki = Pki::new();
    let listener = tokio::net::TcpListener::bind((terminate_support::LOCAL, 0))
        .await
        .unwrap();
    let addr = listener.local_addr().unwrap();
    let accepted = Arc::new(AtomicUsize::new(0));
    let kept = Arc::new(std::sync::Mutex::new(Vec::new()));
    let accepting = {
        let accepted = Arc::clone(&accepted);
        let kept = Arc::clone(&kept);
        tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                accepted.fetch_add(1, Ordering::SeqCst);
                kept.lock().unwrap().push(tcp);
            }
        })
    };
    let config = ProxyConfig::default().with_terminate_timeouts(
        Duration::from_secs(1),
        Duration::from_secs(60),
        Duration::from_secs(10),
        Duration::from_secs(10),
    );
    let rig = RigBuilder::new(&pki)
        .name("bound.test", addr)
        .config(config)
        .build();
    let mut guest = rig.guest().await;
    // The handshake's own attempt fails after the TLS timeout; the first stream is told why.
    let client = guest.h2("bound.test:443", &[b"h2"]).await;
    assert_eq!(accepted.load(Ordering::SeqCst), 1);
    let asked = Instant::now();
    let mut tasks = Vec::new();
    for i in 0..5 {
        let mut sender = client.sender.clone();
        tasks.push(tokio::spawn(async move {
            let request = http::Request::builder()
                .uri(format!("https://bound.test/{i}"))
                .body(full(Bytes::new()))
                .unwrap();
            terminate_support::h2_rig::collect(sender.send_request(request).await.unwrap()).await
        }));
    }
    for task in tasks {
        assert_eq!(task.await.unwrap().status, 504);
    }
    assert!(
        asked.elapsed() < Duration::from_millis(3500),
        "five streams cost {:?}, not one attempt's time",
        asked.elapsed()
    );
    assert_eq!(
        accepted.load(Ordering::SeqCst),
        2,
        "one attempt for all five streams, besides the handshake's"
    );
    // The failure is not remembered: the next stream tries again.
    let mut sender = client.sender.clone();
    let request = http::Request::builder()
        .uri("https://bound.test/later")
        .body(full(Bytes::new()))
        .unwrap();
    let got = terminate_support::h2_rig::collect(sender.send_request(request).await.unwrap()).await;
    assert_eq!(got.status, 504);
    assert_eq!(accepted.load(Ordering::SeqCst), 3);
    accepting.abort();
}

// ---------------------------------------------------------------------------------------------
// The guest leg's windows

fn be32(bytes: &[u8]) -> Option<u32> {
    Some(u32::from_be_bytes(bytes.try_into().ok()?))
}

/// The value of SETTINGS parameter `id` in the first non-empty SETTINGS frame of `frames`.
fn setting(frames: &[(u8, u32, Vec<u8>)], id: u16) -> Option<u32> {
    let (_, _, payload) = frames
        .iter()
        .find(|(kind, stream, payload)| *kind == 4 && *stream == 0 && !payload.is_empty())?;
    payload.chunks(6).find_map(|entry| {
        let (key, value) = entry.split_at_checked(2)?;
        (u16::from_be_bytes(key.try_into().ok()?) == id)
            .then(|| be32(value))
            .flatten()
    })
}

#[tokio::test]
async fn the_guest_leg_announces_fixed_windows() {
    let pki = Pki::new();
    let server = H2Server::recording(&pki, "bound.test", ok()).await;
    let rig = RigBuilder::new(&pki)
        .name("bound.test", server.addr)
        .build();
    let mut guest = rig.guest().await;
    let mut tls = raw(&mut guest).await;
    let (opening, _) = frames_until_end(&mut tls, Duration::from_millis(500)).await;
    assert_eq!(
        setting(&opening, 4),
        Some(512 * 1024),
        "a stream's window is fixed at 512 KiB: {opening:?}"
    );
    // The connection window is raised once, at the start, to 2 MiB; an adaptive window would
    // start at the protocol's 64 KiB and be raised again as data flows.
    let grants: Vec<u32> = opening
        .iter()
        .filter(|(kind, stream, _)| *kind == 8 && *stream == 0)
        .filter_map(|(_, _, payload)| be32(payload))
        .collect();
    assert_eq!(grants, [2 * 1024 * 1024 - 65_535], "{opening:?}");
}

// ---------------------------------------------------------------------------------------------
// WebSocket

const KEY: &str = "dGhlIHNhbXBsZSBub25jZQ==";

#[tokio::test]
async fn an_agreed_websocket_over_http11_that_goes_silent_is_closed_and_recorded() {
    let pki = Pki::new();
    let server = WsServer::start(&pki, "bound.test").await;
    let config = ProxyConfig::default().with_idle_limits(
        Duration::from_secs(60),
        Duration::from_secs(1),
        Duration::from_secs(20),
    );
    let rig = RigBuilder::new(&pki)
        .name("bound.test", server.addr)
        .config(config)
        .build();
    let mut guest = rig.guest().await;
    let mut client = guest.tls("bound.test:443", None).await.unwrap();
    client
        .send(
            format!(
                "GET /chat HTTP/1.1\r\nHost: bound.test\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Key: {KEY}\r\nSec-WebSocket-Version: 13\r\n\r\n"
            )
            .as_bytes(),
        )
        .await;
    loop {
        let mut line = String::new();
        tokio::io::AsyncBufReadExt::read_line(&mut client.stream, &mut line)
            .await
            .unwrap();
        if line == "\r\n" {
            break;
        }
    }
    // Traffic keeps it open past the limit...
    for _ in 0..3 {
        tokio::time::sleep(Duration::from_millis(600)).await;
        client
            .stream
            .write_all(b"\x81\x85mask!hello")
            .await
            .unwrap();
        let mut echo = [0_u8; 12];
        client.stream.read_exact(&mut echo).await.unwrap();
    }
    // ...and silence ends it.
    let mut rest = Vec::new();
    let read = tokio::time::timeout(
        Duration::from_secs(10),
        client.stream.read_to_end(&mut rest),
    )
    .await;
    assert!(read.is_ok(), "the silent WebSocket was never closed");
    let events = rig.events(1).await;
    assert_eq!(
        reasons(&events),
        [ConnectionReason::LimitEnded("websocket_idle")]
    );
}

#[tokio::test]
async fn an_agreed_websocket_over_http2_that_goes_silent_is_closed_and_recorded() {
    let pki = Pki::new();
    let server = WsServer::start(&pki, "bound.test").await;
    let config = ProxyConfig::default().with_idle_limits(
        Duration::from_secs(60),
        Duration::from_secs(1),
        Duration::from_secs(20),
    );
    let rig = RigBuilder::new(&pki)
        .name("bound.test", server.addr)
        .config(config)
        .build();
    let mut guest = rig.guest().await;
    let mut client = guest.h2("bound.test:443", &[b"h2"]).await;
    let request = http::Request::builder()
        .method("CONNECT")
        .uri("https://bound.test/chat")
        .header("sec-websocket-version", "13")
        .extension(hyper::ext::Protocol::from_static("websocket"))
        .body(open_body())
        .unwrap();
    let mut response = client.sender.send_request(request).await.unwrap();
    assert_eq!(response.status(), 200);
    let upgraded = hyper::upgrade::on(&mut response).await.unwrap();
    let mut io = hyper_util::rt::TokioIo::new(upgraded);
    io.write_all(b"hello").await.unwrap();
    let mut echo = [0_u8; 5];
    io.read_exact(&mut echo).await.unwrap();
    let mut rest = Vec::new();
    let read = tokio::time::timeout(Duration::from_secs(10), io.read_to_end(&mut rest)).await;
    assert!(read.is_ok(), "the silent WebSocket was never closed");
    drop(io);
    client.close().await;
    let events = rig.events(1).await;
    assert_eq!(
        reasons(&events),
        [ConnectionReason::LimitEnded("websocket_idle")]
    );
}

// ---------------------------------------------------------------------------------------------
// The audit says which limit ended a connection

#[tokio::test]
async fn a_control_frame_flood_is_recorded_as_the_limit_that_ended_the_connection() {
    let pki = Pki::new();
    let server = H2Server::recording(&pki, "bound.test", ok()).await;
    let rig = RigBuilder::new(&pki)
        .name("bound.test", server.addr)
        .build();
    let mut guest = rig.guest().await;
    let mut tls = raw(&mut guest).await;
    let mut pings = Vec::new();
    for _ in 0..1100 {
        pings.extend(frame(6, 0, 0, &[0; 8]));
    }
    let _ = tls.write_all(&pings).await;
    let (_, closed) = frames_until_end(&mut tls, Duration::from_secs(10)).await;
    assert!(closed);
    let events = rig.events(1).await;
    assert_eq!(
        reasons(&events),
        [ConnectionReason::LimitEnded("control_frame_flood")]
    );
}

#[tokio::test]
async fn a_header_block_left_open_is_recorded_as_the_limit_that_ended_the_connection() {
    let pki = Pki::new();
    let server = H2Server::recording(&pki, "bound.test", ok()).await;
    let config = ProxyConfig::default().with_head_timeout(Duration::from_secs(1));
    let rig = RigBuilder::new(&pki)
        .name("bound.test", server.addr)
        .config(config)
        .build();
    let mut guest = rig.guest().await;
    let mut tls = raw(&mut guest).await;
    ack_server_settings(&mut tls).await;
    tls.write_all(&frame(1, 0x1, 1, &get_block(b"/")))
        .await
        .unwrap();
    let (_, closed) = frames_until_end(&mut tls, Duration::from_secs(10)).await;
    assert!(closed);
    let events = rig.events(1).await;
    assert_eq!(
        reasons(&events),
        [ConnectionReason::LimitEnded("header_block_too_slow")]
    );
}
