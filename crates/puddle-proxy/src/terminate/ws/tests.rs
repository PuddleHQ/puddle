// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn the_accept_key_is_the_one_of_rfc_6455() {
    // RFC 6455 §1.3's example.
    assert_eq!(
        accept_for("dGhlIHNhbXBsZSBub25jZQ=="),
        "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
    );
}

#[test]
fn only_a_single_websocket_upgrade_with_the_connection_token_counts() {
    let upgrade = vec!["keep-alive".to_owned(), "upgrade".to_owned()];
    assert!(is_upgrade_request(&upgrade, &["WebSocket"]));
    assert!(!is_upgrade_request(&upgrade, &["websocket", "h2c"]));
    assert!(!is_upgrade_request(&upgrade, &["h2c"]));
    assert!(!is_upgrade_request(
        &["keep-alive".to_owned()],
        &["websocket"]
    ));
    assert!(!is_upgrade_request(&upgrade, &[]));
}

#[test]
fn a_new_key_is_sixteen_random_bytes_in_base64() {
    let a = new_key().unwrap();
    let b = new_key().unwrap();
    assert_eq!(STANDARD.decode(&a).unwrap().len(), 16);
    assert_ne!(a, b);
}

#[test]
fn the_answer_keeps_end_to_end_headers_only() {
    let mut headers = HeaderMap::new();
    headers.insert(
        ::http::header::CONNECTION,
        HeaderValue::from_static("Upgrade"),
    );
    headers.insert(
        ::http::header::UPGRADE,
        HeaderValue::from_static("websocket"),
    );
    headers.insert("sec-websocket-accept", HeaderValue::from_static("x"));
    headers.insert("sec-websocket-protocol", HeaderValue::from_static("chat"));
    let kept: Vec<_> = answer_headers(&headers, false)
        .into_iter()
        .map(|(n, _)| n.to_string())
        .collect();
    assert_eq!(kept, ["sec-websocket-protocol"]);
    assert_eq!(answer_headers(&headers, true).len(), 2);
}

#[test]
fn a_header_is_single_only_when_it_has_exactly_one_text_value() {
    let mut headers = HeaderMap::new();
    let name = HeaderName::from_static("sec-websocket-protocol");
    assert_eq!(single(&headers, &name), None);
    headers.append(name.clone(), "chat".parse().unwrap());
    assert_eq!(single(&headers, &name), Some("chat"));
    headers.append(name.clone(), "superchat".parse().unwrap());
    assert_eq!(single(&headers, &name), None);
}

/// A writer that notes that it was flushed.
#[derive(Default)]
struct Sink {
    flushed: bool,
}

impl AsyncWrite for Sink {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut TaskContext<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(mut self: Pin<&mut Self>, _cx: &mut TaskContext<'_>) -> Poll<io::Result<()>> {
        self.flushed = true;
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut TaskContext<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

#[tokio::test]
async fn a_flush_reaches_the_stream_it_wraps() {
    use tokio::io::AsyncWriteExt as _;
    let activity = Arc::new(Activity::new(Duration::from_secs(5), Arc::default()));
    let mut moving = Moving::new(Sink::default(), &activity);
    moving.write_all(b"x").await.unwrap();
    moving.flush().await.unwrap();
    moving.shutdown().await.unwrap();
    assert!(moving.inner.flushed);
}

#[tokio::test(start_paused = true)]
async fn only_a_read_that_brought_bytes_counts_as_movement() {
    use tokio::io::AsyncReadExt as _;
    let activity = Arc::new(Activity::new(Duration::from_secs(60), Arc::default()));
    let quiet = Duration::from_secs(5);
    tokio::time::advance(quiet).await;
    let mut buf = [0_u8; 8];
    let mut end = Moving::new(&b""[..], &activity);
    assert_eq!(end.read(&mut buf).await.unwrap(), 0);
    assert_eq!(
        activity.quiet_for(),
        quiet,
        "the end of the stream is no movement"
    );
    let mut data = Moving::new(&b"x"[..], &activity);
    assert_eq!(data.read(&mut buf).await.unwrap(), 1);
    assert_eq!(activity.quiet_for(), Duration::ZERO);
}
