// SPDX-License-Identifier: AGPL-3.0-or-later
use std::time::Duration;

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncReadExt, ReadBuf};

use super::*;

async fn capture(bytes: Vec<u8>, early: &[u8]) -> FirstBytes {
    let (mut a, b) = tokio::io::duplex(64 * 1024);
    tokio::io::AsyncWriteExt::write_all(&mut a, &bytes)
        .await
        .unwrap();
    let mut reader = b;
    let wait = Duration::from_millis(200);
    let result = capture_first_record(&mut reader, early.to_vec(), wait, wait).await;
    drop(a);
    result.unwrap()
}

/// A stream whose read fails, as one the guest reset does.
struct Reset;

impl AsyncRead for Reset {
    fn poll_read(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
        _: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Poll::Ready(Err(io::ErrorKind::ConnectionReset.into()))
    }
}

#[tokio::test]
async fn a_read_error_before_a_whole_record_is_an_error_not_a_pass_through() {
    let wait = Duration::from_millis(200);
    let err = capture_first_record(&mut Reset, Vec::new(), wait, wait)
        .await
        .unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::ConnectionReset);
}

#[tokio::test]
async fn a_client_hello_is_captured_whole_and_read_without_a_connection() {
    let hello = test_client_hello("free.test", &[b"h2", b"http/1.1"]);
    assert_eq!(
        capture(hello.clone(), &[]).await,
        FirstBytes::Tls(hello.clone())
    );
    let host = Host::parse_normalised("free.test").unwrap();
    let read = parse_hello(&hello, &host).unwrap();
    assert!(read.name_matches && read.offers_h2 && read.offers_h1);
    let other = Host::parse_normalised("other.test").unwrap();
    assert!(!parse_hello(&hello, &other).unwrap().name_matches);
    let h2_only = test_client_hello("free.test", &[b"h2"]);
    let read = parse_hello(&h2_only, &host).unwrap();
    assert!(read.offers_h2 && !read.offers_h1);
}

#[tokio::test]
async fn bytes_read_before_the_capture_count_as_the_start_of_the_record() {
    let hello = test_client_hello("free.test", &[]);
    let (early, rest) = hello.split_at(3);
    assert_eq!(
        capture(rest.to_vec(), early).await,
        FirstBytes::Tls(hello.clone())
    );
    assert_eq!(capture(Vec::new(), &hello).await, FirstBytes::Tls(hello));
}

#[tokio::test]
async fn anything_that_is_not_one_whole_client_hello_is_other_and_keeps_its_bytes() {
    let hello = test_client_hello("free.test", &[]);
    let cases: Vec<Vec<u8>> = vec![
        b"GET / HTTP/1.1

"
        .to_vec(),
        b"SSH".to_vec(),
        Vec::new(),
        // A handshake record that is not a ClientHello.
        vec![0x16, 3, 3, 0, 4, 2, 0, 0, 0],
        // Application data, a zero-length record, a record over 16 KiB.
        vec![0x17, 3, 3, 0, 1, 0],
        vec![0x16, 3, 3, 0, 0],
        vec![0x16, 3, 3, 0x7f, 0xff],
        // A record cut short.
        hello[..hello.len() - 1].to_vec(),
    ];
    for bytes in cases {
        assert_eq!(capture(bytes.clone(), &[]).await, FirstBytes::Other(bytes));
    }
}

#[test]
fn bytes_that_are_not_a_whole_client_hello_give_no_hello() {
    let host = Host::parse_normalised("free.test").unwrap();
    let hello = test_client_hello("free.test", &[]);
    assert!(parse_hello(&hello[..hello.len() - 1], &host).is_none());
    assert!(parse_hello(b"GET / HTTP/1.1\r\n\r\n", &host).is_none());
    assert!(parse_hello(&[], &host).is_none());
}

#[test]
fn a_scan_tells_a_whole_hello_from_a_cut_one_and_from_another_protocol() {
    let hello = test_client_hello("free.test", &[]);
    assert_eq!(
        scan_hello(&hello),
        Scan::Name(Host::parse_normalised("free.test").ok())
    );
    assert_eq!(scan_hello(&hello[..hello.len() - 1]), Scan::More);
    assert_eq!(scan_hello(&[]), Scan::More);
    assert_eq!(scan_hello(b"GET / HTTP/1.1\r\n\r\n"), Scan::Other);
    assert_eq!(scan_hello(&[0x17, 3, 3, 0, 1, 0]), Scan::Other);
}

#[tokio::test]
async fn the_prefix_is_read_before_the_stream() {
    let (mut a, b) = tokio::io::duplex(64);
    tokio::io::AsyncWriteExt::write_all(&mut a, b"-tail")
        .await
        .unwrap();
    drop(a);
    let mut s = Prefixed::new(b, b"head".to_vec());
    let mut out = String::new();
    s.read_to_string(&mut out).await.unwrap();
    assert_eq!(out, "head-tail");
}

fn record(kind: u8, major: u8, len: usize) -> Vec<u8> {
    let [hi, lo] = u16::try_from(len).unwrap().to_be_bytes();
    let mut out = vec![kind, major, 3, hi, lo, 1];
    out.resize(5 + len, 0);
    out
}

#[tokio::test]
async fn only_a_handshake_record_of_at_most_sixteen_kib_is_a_tls_start() {
    let whole = record(0x16, 3, 16 * 1024);
    assert_eq!(capture(whole.clone(), &[]).await, FirstBytes::Tls(whole));
    let over = record(0x16, 3, 16 * 1024 + 1);
    assert!(matches!(capture(over, &[]).await, FirstBytes::Other(_)));
    let data = record(0x17, 3, 16);
    assert_eq!(capture(data.clone(), &[]).await, FirstBytes::Other(data));
    let old = record(0x16, 2, 16);
    assert_eq!(capture(old.clone(), &[]).await, FirstBytes::Other(old));
}

/// A writer that notes that it was flushed and shut down.
#[derive(Default)]
struct Sink {
    flushed: bool,
    shut: bool,
}

impl AsyncWrite for Sink {
    fn poll_write(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(mut self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.flushed = true;
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.shut = true;
        Poll::Ready(Ok(()))
    }
}

#[tokio::test]
async fn a_flush_and_a_shutdown_reach_the_stream_under_the_prefix() {
    use tokio::io::AsyncWriteExt as _;
    let mut prefixed = Prefixed::new(Sink::default(), b"x".to_vec());
    prefixed.write_all(b"y").await.unwrap();
    prefixed.flush().await.unwrap();
    prefixed.shutdown().await.unwrap();
    assert!(prefixed.inner.flushed && prefixed.inner.shut);
}
