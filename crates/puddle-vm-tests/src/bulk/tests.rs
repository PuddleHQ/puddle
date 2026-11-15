// SPDX-License-Identifier: AGPL-3.0-or-later
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
        "the host no longer answering",
        std::time::Duration::from_secs(5),
        // Not "the port refuses connections": the host bound port 0 and gave the number back, so another
        // test's socket can hold it by now. A request the host would answer with 400 shows it is gone.
        || async move {
            let status =
                tokio::task::spawn_blocking(move || puddle_test_kit::http_status(addr, "/", ""))
                    .await
                    .unwrap();
            (status != Some(400)).then_some(())
        },
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
    // The server may close on an oversized head while the rest is still being sent.
    let _ = tcp.write_all(request).await;
    // Half-close: a request that is not complete ends instead of waiting for more.
    let _ = tcp.shutdown().await;
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
async fn a_client_that_sends_nothing_and_leaves_is_let_go() {
    let host = BulkHost::start(1).await.unwrap();
    // Closing our sending half with no request: the server must end the connection (EOF here).
    assert_eq!(send_raw(host.addr(), b"").await, b"");
}

#[tokio::test]
async fn a_client_that_leaves_before_its_body_does_not_count_as_served() {
    let host = BulkHost::start(1).await.unwrap();
    let mut tcp = ask(host.addr(), "/gate/1073741824").await;
    // The head says what is coming; then we leave with a gibibyte unread.
    let head = read_reply(&mut tcp, 0).await;
    assert!(head.starts_with(b"HTTP/1.1 200 OK"));
    drop(tcp);
    // The server's writes fail once the reset arrives; a later client is served as usual.
    let reply = get(host.addr(), "/gate/10").await;
    assert_eq!(body_of(&reply).len(), 10);
    assert_eq!(host.served(), 1);
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
