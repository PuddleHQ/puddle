// SPDX-License-Identifier: AGPL-3.0-or-later
use tokio::net::TcpListener;

use super::*;

const SHORT: Duration = Duration::from_millis(300);

async fn serve(parts: Vec<Vec<u8>>, pause: Duration) -> (url::Url, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = url::Url::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
    let task = tokio::spawn(async move {
        let (mut conn, _) = listener.accept().await.unwrap();
        let mut seen = [0_u8; 4096];
        let _ = conn.read(&mut seen).await;
        // flake-ok: a scripted server pacing its writes so the client sees separate reads; it waits for nothing.
        for part in parts {
            conn.write_all(&part).await.unwrap();
            conn.flush().await.unwrap();
            tokio::time::sleep(pause).await;
        }
        // Hold the connection open: the test decides when it is over.
        tokio::time::sleep(Duration::from_secs(30)).await;
    });
    (url, task)
}

#[test]
fn dechunk_joins_chunks_and_counts_sizes_in_bytes() {
    // "é" is two bytes; the size says so and the body holds it whole.
    let body = "1\r\na\r\n2\r\n\u{e9}\r\n3;ext=1\r\nbcd\r\n0\r\n\r\n";
    assert_eq!(dechunk(body.as_bytes()).unwrap(), "a\u{e9}bcd".as_bytes());
}

#[test]
fn dechunk_refuses_a_malformed_size_instead_of_truncating() {
    let err = dechunk(b"1\r\na\r\nzz\r\nbb\r\n0\r\n\r\n").unwrap_err();
    assert!(err.contains("malformed chunk size"), "{err}");
}

#[test]
fn dechunk_refuses_a_body_that_stops_before_the_last_chunk() {
    let err = dechunk(b"3\r\nabc\r\n5\r\nab").unwrap_err();
    assert!(err.contains("before its last chunk"), "{err}");
}

#[test]
fn dechunk_refuses_a_chunk_without_its_crlf() {
    let err = dechunk(b"3\r\nabcXX0\r\n\r\n").unwrap_err();
    assert!(err.contains("not followed by CRLF"), "{err}");
}

#[test]
fn a_reply_is_split_on_bytes_so_a_non_utf8_head_cannot_shift_the_body() {
    let mut raw = b"HTTP/1.1 200 OK\r\nX-Junk: ".to_vec();
    raw.extend_from_slice(&[0xff, 0xfe, 0xff]);
    raw.extend_from_slice(b"\r\n\r\n{\"a\":1}");
    let reply = parse_reply(&raw).unwrap();
    assert_eq!((reply.status, reply.body.as_str()), (200, "{\"a\":1}"));
}

#[test]
fn a_chunked_reply_is_reassembled_and_a_broken_one_is_an_error() {
    let ok = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nhi\r\n0\r\n\r\n";
    assert_eq!(parse_reply(ok).unwrap().body, "hi");
    let bad = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nq\r\nhi\r\n0\r\n\r\n";
    assert!(
        parse_reply(bad)
            .unwrap_err()
            .contains("malformed chunk size")
    );
}

#[tokio::test]
#[should_panic(expected = "no complete answer within")]
async fn an_exchange_that_never_finishes_panics() {
    let (url, _server) = serve(
        vec![b"HTTP/1.1 200 OK\r\n\r\npart".to_vec()],
        Duration::ZERO,
    )
    .await;
    let api = Api::new(&url, "t");
    let head = format!("GET /api/stalls HTTP/1.1\r\nHost: {}\r\n\r\n", api.addr);
    api.exchange(head.as_bytes(), b"", SHORT).await;
}

#[tokio::test]
async fn events_decodes_a_character_split_across_reads() {
    let record = "data: {\"name\":\"x\u{e9}y\"}\n\n".as_bytes().to_vec();
    let at = record.iter().position(|b| *b == 0xc3).unwrap() + 1;
    let parts = vec![record[..at].to_vec(), record[at..].to_vec()];
    let (url, _server) = serve(parts, Duration::from_millis(100)).await;
    let mut events = Events::over(
        TcpStream::connect(url.socket_addrs(|| None).unwrap()[0])
            .await
            .unwrap(),
    );
    // The server reads one request first.
    events.stream.write_all(b"go").await.unwrap();
    let event = events.until(|_| true, Duration::from_secs(5)).await;
    assert_eq!(event["name"], "x\u{e9}y");
}

#[tokio::test]
async fn events_skips_a_keep_alive_comment_and_reads_chunked_framing() {
    let head = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
    let ka = b": ka\n\n";
    let rec = "data: {\"n\":\"\u{e9}\"}\n\n".as_bytes();
    // The record is split in the middle of the two-byte character, across two chunks.
    let cut = rec.iter().position(|b| *b == 0xc3).unwrap() + 1;
    let mut framed = Vec::new();
    for piece in [&ka[..], &rec[..cut], &rec[cut..]] {
        framed.extend_from_slice(format!("{:x}\r\n", piece.len()).as_bytes());
        framed.extend_from_slice(piece);
        framed.extend_from_slice(b"\r\n");
    }
    let (url, _server) = serve(vec![head, framed], Duration::from_millis(50)).await;
    let api = Api::new(&url, "t");
    let mut events = api.events().await;
    let event = events.until(|_| true, Duration::from_secs(5)).await;
    assert_eq!(event["n"], "\u{e9}");
    assert_eq!(events.seen.len(), 1, "the comment is not an event");
}

#[tokio::test]
#[should_panic(expected = "event data is not JSON")]
async fn events_panics_on_a_data_record_that_is_not_json() {
    let (url, _server) = serve(
        vec![b"data: {\"ok\":1}\n\ndata: not json\n\n".to_vec()],
        Duration::ZERO,
    )
    .await;
    let mut events = Events::over(
        TcpStream::connect(url.socket_addrs(|| None).unwrap()[0])
            .await
            .unwrap(),
    );
    events.stream.write_all(b"go").await.unwrap();
    events
        .until(|e| e["never"] == 1, Duration::from_secs(5))
        .await;
}
