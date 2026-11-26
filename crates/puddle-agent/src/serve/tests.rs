// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn a_request_line_gets_the_origin_header_right_after_it() {
    let marked = mark_request(
        b"CONNECT registry.example:443 HTTP/1.1\r\nHost: registry.example:443\r\n\r\n",
        true,
        None,
    );
    assert_eq!(
            marked,
            b"CONNECT registry.example:443 HTTP/1.1\r\nx-puddle-origin: container\r\nHost: registry.example:443\r\n\r\n"
        );
    // A bare line feed ends a request line too, and what follows the line is kept.
    assert_eq!(
        mark_request(
            b"CONNECT a.example:443 HTTP/1.0\nHost: a.example\n\n",
            true,
            None
        ),
        b"CONNECT a.example:443 HTTP/1.0\nx-puddle-origin: container\r\nHost: a.example\n\n"
    );
}

#[tokio::test]
async fn a_request_line_that_arrives_in_pieces_is_marked_and_what_follows_it_is_kept() {
    let (mut client, mut guest) = tokio::io::duplex(64);
    let sender = tokio::spawn(async move {
        guest.write_all(b"CONNECT a.example:4").await.unwrap();
        tokio::task::yield_now().await;
        guest
            .write_all(b"43 HTTP/1.1\r\nHost: a.example\r\n\r\n")
            .await
            .unwrap();
    });
    let mut up = Vec::new();
    send_marked(&mut client, &mut up, true, None).await.unwrap();
    sender.await.unwrap();
    assert!(
        up.starts_with(b"CONNECT a.example:443 HTTP/1.1\r\nx-puddle-origin: container\r\n"),
        "{}",
        String::from_utf8_lossy(&up)
    );
}

#[tokio::test]
async fn a_client_that_closes_before_its_line_ends_sends_what_it_had_unmarked() {
    let (mut client, mut guest) = tokio::io::duplex(64);
    guest.write_all(b"CONNECT a.exa").await.unwrap();
    drop(guest);
    let mut up = Vec::new();
    send_marked(&mut client, &mut up, true, None).await.unwrap();
    assert_eq!(up, b"CONNECT a.exa");
}

#[tokio::test]
async fn a_write_that_fails_is_an_error_not_a_hang() {
    // The host's end of the stream is gone: its writes fail.
    let (mut up, gone) = tokio::io::duplex(8);
    drop(gone);
    let (mut client, mut guest) = tokio::io::duplex(64);
    guest
        .write_all(b"CONNECT a.example:443 HTTP/1.1\r\n\r\n")
        .await
        .unwrap();
    assert!(send_marked(&mut client, &mut up, true, None).await.is_err());
}

#[test]
fn anything_that_is_not_an_http_1_request_line_goes_up_as_it_is() {
    for first in [
        &b""[..],
        b"CONNECT a.example:443 HTTP/1.1",
        b"\x16\x03\x01\x02\x00\n",
        b"SSH-2.0-OpenSSH_9.6\r\n",
        b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n",
        // A plain request goes to its own server, which must not get the header.
        b"GET http://a.example/ HTTP/1.1\r\nHost: a.example\r\n\r\n",
    ] {
        assert_eq!(mark_request(first, true, None), first);
    }
}

#[test]
fn a_known_program_is_named_after_the_request_line_and_a_header_the_client_sent_is_removed() {
    let pip = Program::sanitise("pip").unwrap();
    let marked = mark_request(
        b"CONNECT a.example:443 HTTP/1.1\r\nHost: a.example:443\r\nX-Puddle-Program: forged\r\nx-puddle-program : again\r\n\r\n",
        false,
        Some(&pip),
    );
    assert_eq!(
        marked,
        b"CONNECT a.example:443 HTTP/1.1\r\nx-puddle-program: pip\r\nHost: a.example:443\r\n\r\n"
    );
}

#[test]
fn a_client_sent_program_header_is_removed_even_when_no_program_is_known() {
    let marked = mark_request(
        b"CONNECT a.example:443 HTTP/1.1\r\nx-puddle-program: forged\r\nHost: a.example:443\r\n\r\n",
        false,
        None,
    );
    assert_eq!(
        marked,
        b"CONNECT a.example:443 HTTP/1.1\r\nHost: a.example:443\r\n\r\n"
    );
}

#[test]
fn bytes_after_the_head_are_not_touched() {
    let marked = mark_request(
        b"CONNECT a.example:443 HTTP/1.1\r\n\r\nx-puddle-program: in the tunnel\r\n",
        false,
        None,
    );
    assert_eq!(
        marked,
        b"CONNECT a.example:443 HTTP/1.1\r\n\r\nx-puddle-program: in the tunnel\r\n"
    );
}

#[test]
fn a_container_connection_gets_both_headers_in_a_fixed_order() {
    let pip = Program::sanitise("pip").unwrap();
    let marked = mark_request(b"CONNECT a.example:443 HTTP/1.1\r\n\r\n", true, Some(&pip));
    assert_eq!(
        marked,
        b"CONNECT a.example:443 HTTP/1.1\r\nx-puddle-origin: container\r\nx-puddle-program: pip\r\n\r\n"
    );
}

#[tokio::test]
async fn a_head_that_arrives_in_pieces_is_read_to_its_blank_line() {
    let (mut client, mut guest) = tokio::io::duplex(64);
    let sender = tokio::spawn(async move {
        guest
            .write_all(b"CONNECT a.example:443 HTTP/1.1\r\nx-puddle-pro")
            .await
            .unwrap();
        tokio::task::yield_now().await;
        guest
            .write_all(b"gram: forged\r\n\r\ntunnel bytes")
            .await
            .unwrap();
    });
    let first = request_start(&mut client).await;
    sender.await.unwrap();
    assert!(first.ends_with(b"\r\n\r\ntunnel bytes") || first.ends_with(b"\r\n\r\n"));
    assert!(head_complete(&first));
}

#[tokio::test]
async fn a_request_that_is_not_a_connect_is_not_held_for_its_head() {
    let (mut client, mut guest) = tokio::io::duplex(64);
    guest.write_all(b"\x16\x03\x01\x02\x00\n").await.unwrap();
    let first = tokio::time::timeout(Duration::from_secs(1), request_start(&mut client))
        .await
        .expect("a TLS hello must not wait for a blank line");
    assert_eq!(first, b"\x16\x03\x01\x02\x00\n");
    // Not even a whole first line: the first bytes already say it is no CONNECT.
    guest.write_all(b"GET http://a").await.unwrap();
    let first = tokio::time::timeout(Duration::from_secs(1), request_start(&mut client))
        .await
        .expect("a GET must not wait for a newline");
    assert_eq!(first, b"GET http://a");
}
