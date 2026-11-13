// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

fn frame(kind: u8, flags: u8, stream: u32, payload: &[u8]) -> Vec<u8> {
    let [_, a, b, c] = u32::try_from(payload.len()).unwrap().to_be_bytes();
    let mut out = vec![a, b, c, kind, flags];
    out.extend_from_slice(&stream.to_be_bytes());
    out.extend_from_slice(payload);
    out
}

fn start() -> Vec<u8> {
    b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n".to_vec()
}

#[test]
fn a_header_block_is_open_from_headers_to_end_headers_whatever_the_read_sizes() {
    let mut bytes = start();
    bytes.extend(frame(HEADERS, 0, 1, b"abc"));
    bytes.extend(frame(CONTINUATION, 0, 1, b"def"));
    let mut open = Frames::new();
    let now = Instant::now();
    open.observe(&bytes, now).unwrap();
    assert_eq!(open.block_open_since(), Some(now));
    open.observe(&frame(CONTINUATION, END_HEADERS, 1, b"g"), now)
        .unwrap();
    assert_eq!(open.block_open_since(), None);
    // Byte by byte gives the same answer.
    let mut piecewise = Frames::new();
    for byte in &bytes {
        piecewise.observe(&[*byte], now).unwrap();
    }
    assert_eq!(piecewise.block_open_since(), Some(now));
    for byte in frame(CONTINUATION, END_HEADERS, 1, b"g") {
        piecewise.observe(&[byte], now).unwrap();
    }
    assert_eq!(piecewise.block_open_since(), None);
}

#[test]
fn a_frame_header_split_across_reads_is_completed_without_eating_the_payload() {
    let now = Instant::now();
    let mut frames = Frames::new();
    frames.observe(&start(), now).unwrap();
    let first = frame(HEADERS, 0, 1, b"abc");
    // Four bytes of the header, then the rest of it with the payload and the next frame.
    frames.observe(&first[..4], now).unwrap();
    let mut rest = first[4..].to_vec();
    rest.extend(frame(CONTINUATION, END_HEADERS, 1, b"def"));
    frames.observe(&rest, now).unwrap();
    assert_eq!(
        frames.block_open_since(),
        None,
        "the continuation that ends the block was read as a frame of its own"
    );
}

#[test]
fn a_frame_length_uses_all_three_length_bytes() {
    let now = Instant::now();
    let mut frames = Frames::new();
    frames.observe(&start(), now).unwrap();
    // 0x010203 bytes: every byte of the length is nonzero and differs from the others.
    let mut wire = frame(DATA, END_STREAM, 1, &vec![0; 0x01_0203]);
    wire.extend(frame(HEADERS, 0, 3, b"abc"));
    frames.observe(&wire, now).unwrap();
    assert_eq!(
        frames.block_open_since(),
        Some(now),
        "the headers frame after the long frame was not read at its boundary"
    );
}

#[test]
fn a_block_that_ends_in_the_same_frame_stays_open_until_its_payload_is_in() {
    let mut frames = Frames::new();
    let now = Instant::now();
    let mut bytes = start();
    let whole = frame(HEADERS, END_HEADERS, 1, b"abcdef");
    bytes.extend_from_slice(&whole[..whole.len() - 2]);
    frames.observe(&bytes, now).unwrap();
    assert!(frames.block_open_since().is_some());
    frames.observe(&whole[whole.len() - 2..], now).unwrap();
    assert!(frames.block_open_since().is_none());
}

#[test]
fn control_frames_over_the_cap_in_the_window_are_a_violation() {
    let mut frames = Frames::new();
    let now = Instant::now();
    frames.observe(&start(), now).unwrap();
    let mut failed_at = None;
    for i in 0..=MAX_CONTROL_FRAMES {
        if frames
            .observe(&frame(RST_STREAM, 0, 1, &[0, 0, 0, 8]), now)
            .is_err()
        {
            failed_at = Some(i);
            break;
        }
    }
    assert_eq!(failed_at, Some(MAX_CONTROL_FRAMES));
}

#[test]
fn the_cap_counts_a_window_not_the_connection() {
    let mut frames = Frames::new();
    let t0 = Instant::now();
    frames.observe(&start(), t0).unwrap();
    for _ in 0..MAX_CONTROL_FRAMES {
        frames.observe(&frame(PING, 0, 0, &[0; 8]), t0).unwrap();
    }
    let later = t0 + WINDOW + Duration::from_secs(1);
    for _ in 0..MAX_CONTROL_FRAMES {
        frames.observe(&frame(PING, 0, 0, &[0; 8]), later).unwrap();
    }
}

#[test]
fn data_settings_acks_and_window_updates_are_not_counted() {
    let mut frames = Frames::new();
    let now = Instant::now();
    frames.observe(&start(), now).unwrap();
    for _ in 0..(MAX_CONTROL_FRAMES * 3) {
        frames.observe(&frame(DATA, 0, 1, b"x"), now).unwrap();
        frames.observe(&frame(SETTINGS, 1, 0, b""), now).unwrap();
        frames.observe(&frame(8, 0, 1, &[0, 0, 1, 0]), now).unwrap();
        frames
            .observe(&frame(DATA, END_STREAM, 1, b""), now)
            .unwrap();
    }
    assert!(
        frames.observe(&frame(DATA, 0, 1, b""), now).is_ok(),
        "one empty data frame is fine"
    );
}

fn flood_fails_at_the_cap(kind: u8, flags: u8, payload: &[u8]) {
    let mut frames = Frames::new();
    let now = Instant::now();
    frames.observe(&start(), now).unwrap();
    for _ in 0..MAX_CONTROL_FRAMES {
        frames
            .observe(&frame(kind, flags, 1, payload), now)
            .unwrap();
    }
    assert!(
        frames
            .observe(&frame(kind, flags, 1, payload), now)
            .is_err(),
        "frame kind {kind} was not counted"
    );
}

#[test]
fn a_flood_of_settings_or_of_empty_data_frames_is_a_violation() {
    flood_fails_at_the_cap(SETTINGS, 0, b"");
    flood_fails_at_the_cap(DATA, 0, b"");
}

#[test]
fn a_frame_exactly_one_window_after_the_first_still_counts_with_it() {
    let mut frames = Frames::new();
    let t0 = Instant::now();
    frames.observe(&start(), t0).unwrap();
    for _ in 0..MAX_CONTROL_FRAMES {
        frames.observe(&frame(PING, 0, 0, &[0; 8]), t0).unwrap();
    }
    assert!(
        frames
            .observe(&frame(PING, 0, 0, &[0; 8]), t0 + WINDOW)
            .is_err()
    );
}

/// A connection whose reads fail.
struct Broken;

impl AsyncRead for Broken {
    fn poll_read(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        _buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Poll::Ready(Err(io::Error::from(io::ErrorKind::ConnectionReset)))
    }
}

#[tokio::test]
async fn a_read_error_of_the_connection_is_passed_on_as_it_is() {
    use tokio::io::AsyncReadExt as _;
    let mut watched = Watch::new(Broken, Duration::from_secs(1), LimitHit::default());
    let err = watched.read(&mut [0_u8; 16]).await.unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::ConnectionReset);
}

#[tokio::test(start_paused = true)]
async fn the_header_deadline_ends_with_the_block_it_was_set_for() {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let (mut guest, proxy_side) = tokio::io::duplex(4096);
    let timeout = Duration::from_secs(5);
    let mut watched = Watch::new(proxy_side, timeout, LimitHit::default());
    let mut buf = [0_u8; 4096];
    let mut open = start();
    open.extend(frame(HEADERS, 0, 1, b"abc"));
    guest.write_all(&open).await.unwrap();
    assert!(watched.read(&mut buf).await.unwrap() > 0);
    guest
        .write_all(&frame(CONTINUATION, END_HEADERS, 1, b"def"))
        .await
        .unwrap();
    assert!(watched.read(&mut buf).await.unwrap() > 0);
    tokio::time::advance(timeout * 2).await;
    assert!(
        tokio::time::timeout(Duration::from_secs(1), watched.read(&mut buf))
            .await
            .is_err(),
        "the finished block's deadline went off"
    );
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
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.flushed = true;
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.shut = true;
        Poll::Ready(Ok(()))
    }
}

impl AsyncRead for Sink {
    fn poll_read(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        _buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Poll::Pending
    }
}

#[tokio::test]
async fn a_flush_and_a_shutdown_reach_the_guest_stream() {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let mut watched = Watch::new(Sink::default(), Duration::from_secs(1), LimitHit::default());
    watched.write_all(b"x").await.unwrap();
    watched.flush().await.unwrap();
    watched.shutdown().await.unwrap();
    assert!(watched.inner.flushed && watched.inner.shut);
    // The sink never has bytes to read: the read waits.
    let mut buf = [0_u8; 1];
    assert!(
        tokio::time::timeout(Duration::from_millis(10), watched.read(&mut buf))
            .await
            .is_err()
    );
}
