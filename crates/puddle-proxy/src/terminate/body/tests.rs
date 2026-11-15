// SPDX-License-Identifier: AGPL-3.0-or-later
use tokio::io::BufReader;

use super::*;

async fn decode(input: &[u8], body: Body) -> io::Result<Vec<u8>> {
    let mut r = BufReader::new(input);
    let mut decoder = BodyReader::new(body);
    let mut out = Vec::new();
    while let Some(piece) = decoder.next(&mut r).await? {
        out.extend_from_slice(&piece);
    }
    Ok(out)
}

#[tokio::test]
async fn length_and_chunked_bodies_decode_and_leave_the_next_request() {
    assert_eq!(
        decode(b"hello world", Body::Length(5)).await.unwrap(),
        b"hello"
    );
    assert_eq!(decode(b"", Body::None).await.unwrap(), b"");
    let chunked = b"3;ext=1\r\nabc\r\n2\r\nde\r\n0\r\nx-trailer: 1\r\n\r\nGET / HTTP/1.1\r\n";
    let mut r = BufReader::new(&chunked[..]);
    let mut decoder = BodyReader::new(Body::Chunked);
    let mut out = Vec::new();
    while let Some(piece) = decoder.next(&mut r).await.unwrap() {
        out.extend_from_slice(&piece);
    }
    assert_eq!(out, b"abcde");
    let mut rest = String::new();
    tokio::io::AsyncReadExt::read_to_string(&mut r, &mut rest)
        .await
        .unwrap();
    assert_eq!(rest, "GET / HTTP/1.1\r\n");
}

#[tokio::test]
async fn broken_framing_is_an_error() {
    for (input, body) in [
        (&b"abc"[..], Body::Length(5)),
        (b"zz\r\nabc\r\n0\r\n\r\n", Body::Chunked),
        (b"3\r\nabcXX0\r\n\r\n", Body::Chunked),
        (b"3\r\nab", Body::Chunked),
        (b"ffffffffffffffffff\r\n", Body::Chunked),
        (b"0\r\n", Body::Chunked),
    ] {
        assert!(decode(input, body).await.is_err(), "{input:?}");
    }
}

#[tokio::test]
async fn a_large_body_is_handed_on_in_pieces_of_64_kib() {
    let input = vec![7u8; 200 * 1024];
    let mut r = BufReader::with_capacity(256 * 1024, &input[..]);
    let mut decoder = BodyReader::new(Body::Length(input.len() as u64));
    let mut sizes = Vec::new();
    while let Some(piece) = decoder.next(&mut r).await.unwrap() {
        sizes.push(piece.len());
    }
    assert_eq!(sizes, [64 * 1024, 64 * 1024, 64 * 1024, 8 * 1024]);
}

/// Trailers of exactly 64 KiB (the blank line counted) are accepted, one byte more is not.
#[tokio::test]
async fn trailers_are_accepted_up_to_64_kib_and_no_further() {
    let trailers = |extra: usize| {
        let mut input = b"0\r\n".to_vec();
        for _ in 0..511 {
            input.extend_from_slice(b"x-a: ");
            input.extend(std::iter::repeat_n(b'a', 121));
            input.extend_from_slice(b"\r\n");
        }
        input.extend_from_slice(b"x-a: ");
        input.extend(std::iter::repeat_n(b'a', 119 + extra));
        input.extend_from_slice(b"\r\n\r\n");
        input
    };
    assert!(decode(&trailers(0), Body::Chunked).await.is_ok());
    assert!(decode(&trailers(1), Body::Chunked).await.is_err());
}

#[tokio::test]
async fn a_chunk_may_end_in_a_bare_line_feed() {
    let input = b"3\nabc\n2\r\nde\n0\n\n";
    assert_eq!(decode(input, Body::Chunked).await.unwrap(), b"abcde");
}

#[tokio::test]
async fn a_channel_body_reports_the_end_of_the_stream_only_when_it_is_over() {
    let (empty, _tx, _abort) = ChannelBody::new(Body::None);
    assert!(empty.is_end_stream());

    let (mut open, tx, _abort) = ChannelBody::new(Body::Length(2));
    assert!(!open.is_end_stream());
    tx.send(Bytes::from_static(b"hi")).await.unwrap();
    drop(tx);
    let frame = std::future::poll_fn(|cx| Pin::new(&mut open).poll_frame(cx)).await;
    assert!(frame.unwrap().unwrap().is_data());
    assert!(!open.is_end_stream(), "the end has not been polled yet");
    assert!(
        std::future::poll_fn(|cx| Pin::new(&mut open).poll_frame(cx))
            .await
            .is_none()
    );
    assert!(open.is_end_stream());

    let (done, _tx, _abort) = ChannelBody::prefetched(Bytes::new());
    assert!(done.is_end_stream());
    let (pending, _tx, _abort) = ChannelBody::prefetched(Bytes::from_static(b"x"));
    assert!(!pending.is_end_stream());
}

#[tokio::test]
async fn endless_trailers_are_cut_off() {
    let mut input = b"0\r\n".to_vec();
    for _ in 0..10_000 {
        input.extend_from_slice(b"x-a: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\r\n");
    }
    assert!(decode(&input, Body::Chunked).await.is_err());
}
