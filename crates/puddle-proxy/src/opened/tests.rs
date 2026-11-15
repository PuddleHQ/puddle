// SPDX-License-Identifier: AGPL-3.0-or-later
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};

use super::*;

const LIMIT: Duration = Duration::from_secs(5);

/// The guest's end and an [`Opened`] over the proxy's end, with `early` as what came behind the head.
fn opened(early: &[u8]) -> (tokio::io::DuplexStream, Opened<tokio::io::DuplexStream>) {
    let (near, far) = tokio::io::duplex(256);
    (
        far,
        Opened {
            stream: near,
            early: early.to_vec(),
            answered: false,
        },
    )
}

#[tokio::test]
async fn what_the_reader_buffered_behind_the_head_becomes_the_early_bytes() {
    let (near, mut far) = tokio::io::duplex(256);
    far.write_all(b"CONNECT a:443 HTTP/1.1\r\n\r\nSSH-2.0-x")
        .await
        .unwrap();
    let mut reader = BufReader::new(near);
    let mut line = String::new();
    while line != "\r\n" {
        line.clear();
        tokio::io::AsyncBufReadExt::read_line(&mut reader, &mut line)
            .await
            .unwrap();
    }
    let opened = Opened::from(reader);
    assert_eq!(opened.early, b"SSH-2.0-x");
    assert!(!opened.answered);
}

#[tokio::test]
async fn bytes_that_tell_already_are_not_answered_and_nothing_more_is_read() {
    for early in [&b"SSH-2.0-x\r\n"[..], b"\x16\x03\x01", b"G"] {
        let (mut far, mut guest) = opened(early);
        far.write_all(b"more").await.unwrap();
        guest.read_first(LIMIT).await;
        assert_eq!(guest.early, early);
        assert!(!guest.answered);
        drop(guest);
        let mut back = Vec::new();
        far.read_to_end(&mut back).await.unwrap();
        assert_eq!(back, b"", "no answer for {early:?}");
    }
}

#[tokio::test]
async fn a_guest_that_sent_nothing_is_answered_and_read_until_its_bytes_tell() {
    let (mut far, mut guest) = opened(b"");
    let client = tokio::spawn(async move {
        let mut answer = vec![0u8; ESTABLISHED.len()];
        far.read_exact(&mut answer).await.unwrap();
        far.write_all(b"SS").await.unwrap();
        far.write_all(b"H-2.0-x\r\n").await.unwrap();
        (answer, far)
    });
    guest.read_first(LIMIT).await;
    let (answer, _far) = client.await.unwrap();
    assert_eq!(answer, ESTABLISHED);
    assert!(guest.answered);
    assert!(ssh::started(&guest.early), "{:?}", guest.early);
}

#[tokio::test]
async fn a_prefix_that_turns_out_otherwise_ends_the_read_and_a_second_answer_is_not_sent() {
    let (mut far, mut guest) = opened(b"S");
    far.write_all(b"QL").await.unwrap();
    guest.read_first(LIMIT).await;
    assert!(guest.early.starts_with(b"SQ"), "{:?}", guest.early);
    guest.answer().await.unwrap();
    drop(guest);
    let mut back = Vec::new();
    far.read_to_end(&mut back).await.unwrap();
    assert_eq!(back, ESTABLISHED, "answered once");
}

#[tokio::test]
async fn the_end_of_the_stream_or_the_limit_ends_the_read_with_what_came() {
    let (mut far, mut guest) = opened(b"SSH");
    far.shutdown().await.unwrap();
    guest.read_first(LIMIT).await;
    assert_eq!(guest.early, b"SSH");

    let (_far, mut guest) = opened(b"");
    guest.read_first(Duration::from_millis(20)).await;
    assert_eq!(guest.early, b"");
    assert!(guest.answered);
}

/// A stream that fails every read and write.
struct Broken;

impl AsyncRead for Broken {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
        _: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        std::task::Poll::Ready(Err(io::ErrorKind::ConnectionReset.into()))
    }
}

impl AsyncWrite for Broken {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
        _: &[u8],
    ) -> std::task::Poll<io::Result<usize>> {
        std::task::Poll::Ready(Err(io::ErrorKind::ConnectionReset.into()))
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
}

#[tokio::test]
async fn a_stream_that_fails_ends_the_read_with_nothing_more_taken() {
    let mut guest = Opened {
        stream: Broken,
        early: b"SS".to_vec(),
        answered: false,
    };
    guest.read_first(LIMIT).await;
    assert_eq!(guest.early, b"SS");
    assert!(!guest.answered, "the answer could not be sent");
    guest.answered = true;
    guest.read_first(LIMIT).await;
    assert_eq!(guest.early, b"SS");
}

#[tokio::test]
async fn a_guest_that_ends_without_bytes_ends_the_read_at_once() {
    let (mut far, mut guest) = opened(b"S");
    // Only the guest's sending side ends: the answer to its `CONNECT` still has a reader.
    far.shutdown().await.unwrap();
    // An end must not be read again until the limit runs out: the outer bound is far under it.
    tokio::time::timeout(
        Duration::from_secs(5),
        guest.read_first(Duration::from_secs(600)),
    )
    .await
    .expect("read_first returns at the end of the stream");
    assert_eq!(guest.early, b"S");
    assert!(guest.answered);
}
