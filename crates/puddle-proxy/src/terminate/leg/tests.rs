// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

/// A server that never answers keeps the upstream driver busy with the request; dropping the
/// connection must cut it, or the socket would stay open past the pool.
#[tokio::test]
async fn dropping_an_h1_connection_closes_its_socket_even_with_a_request_in_flight() {
    use http_body_util::BodyExt as _;
    use tokio::io::AsyncReadExt as _;
    let (client, mut server) = tokio::io::duplex(64 * 1024);
    let (mut sender, connection) = http1::Builder::new()
        .handshake::<_, UpBody>(TokioIo::new(client))
        .await
        .unwrap();
    let driver = tokio::spawn(async move { drop(connection.with_upgrades().await) });
    let request = http::Request::builder()
        .uri("/")
        .header("host", "bound.test")
        .body(
            http_body_util::Empty::new()
                .map_err(|never| match never {})
                .boxed_unsync(),
        )
        .unwrap();
    let pending = tokio::spawn(sender.send_request(request));
    let mut head = [0_u8; 16];
    server.read_exact(&mut head).await.unwrap();
    // The request is in flight (the server has read its head), so the driver would stay on it
    // after the sender is gone; only the drop of the connection cuts it.
    drop(pending);
    let conn = H1Conn {
        sender,
        driver,
        slot: None,
    };
    drop(conn);
    let mut rest = Vec::new();
    let read = tokio::time::timeout(Duration::from_secs(5), server.read_to_end(&mut rest)).await;
    assert!(read.is_ok(), "the socket is still open after the drop");
}

#[test]
fn the_refusals_of_the_upstream_leg_say_what_happened() {
    let busy = busy_refusal("bound.test");
    assert_eq!(busy.status, "503 Service Unavailable");
    assert_eq!(busy.message, "all 8 connections to bound.test are busy");
    let setup = setup_refusal("bound.test", "connection reset");
    assert_eq!(setup.status, "502 Bad Gateway");
    assert_eq!(
        setup.message,
        "could not talk HTTP to bound.test: connection reset"
    );
    assert_eq!(not_offered().status, "502 Bad Gateway");
}

/// Only a server-side failure is remembered for pass-through; a bad name is the guest's own.
#[test]
fn only_server_side_tls_failures_are_passed_through() {
    use puddle_upstream::TlsConnectError as E;
    assert_eq!(
        tls_pass_through(&E::Certificate("x".into())),
        Some("server_certificate")
    );
    assert_eq!(
        tls_pass_through(&E::Handshake("x".into())),
        Some("server_tls")
    );
    assert_eq!(tls_pass_through(&E::InvalidName("x".into())), None);
}
