// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use tokio::io::AsyncWriteExt as _;

#[test]
fn reasons_become_one_printable_line() {
    assert_eq!(
        sanitize_reason("boot hook exited with status 1:\nline two\r\n\tthree"),
        "boot hook exited with status 1: line two three"
    );
    assert_eq!(
        sanitize_reason("\x1b[2J\x1b]0;evil\x07gone"),
        "[2J ]0;evil gone"
    );
    assert_eq!(sanitize_reason("a\u{202E}b\u{200B}c\u{FEFF}"), "a b c");
    assert_eq!(
        sanitize_reason("  leading and trailing  \n"),
        "leading and trailing"
    );
    assert_eq!(sanitize_reason(""), "");
}

#[test]
fn reasons_are_capped() {
    let long = "é".repeat(5000);
    let s = sanitize_reason(&long);
    assert_eq!(s.chars().count(), REASON_LIMIT);
    let spaced = "ab ".repeat(1000);
    assert!(sanitize_reason(&spaced).chars().count() <= REASON_LIMIT);
    assert!(!sanitize_reason(&spaced).ends_with(' '));
}

#[test]
fn the_line_has_the_prefix_and_crlf() {
    assert_eq!(
        refusal_line("workspace \"a\": workspace is not running\n"),
        "puddle-refused: workspace \"a\": workspace is not running\r\n"
    );
}

/// Hands out its bytes one per read.
struct Trickle(std::collections::VecDeque<u8>);

impl AsyncRead for Trickle {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        if let Some(b) = self.0.pop_front() {
            buf.put_slice(&[b]);
        }
        std::task::Poll::Ready(Ok(()))
    }
}

async fn head_of(bytes: &[u8], chunked: bool) -> Head {
    if chunked {
        return read_head(&mut Trickle(bytes.iter().copied().collect()))
            .await
            .unwrap();
    }
    let (mut client, mut server) = tokio::io::duplex(64 * 1024);
    server.write_all(bytes).await.unwrap();
    drop(server);
    read_head(&mut client).await.unwrap()
}

#[tokio::test]
async fn an_ssh_banner_passes_after_the_first_byte_that_differs() {
    assert_eq!(
        head_of(b"SSH-2.0-x\r\n", false).await,
        Head::Pass(b"SSH-2.0-x\r\n".to_vec())
    );
    assert_eq!(
        head_of(b"SSH-2.0-x\r\n", true).await,
        Head::Pass(b"S".to_vec())
    );
    // Shares a start with the prefix, then differs.
    assert_eq!(head_of(b"pudX", true).await, Head::Pass(b"pudX".to_vec()));
}

#[tokio::test]
async fn end_of_stream_passes_what_came() {
    assert_eq!(head_of(b"", false).await, Head::Pass(Vec::new()));
    assert_eq!(head_of(b"pud", true).await, Head::Pass(b"pud".to_vec()));
}

#[tokio::test]
async fn a_refusal_is_read_to_its_line_end_in_any_chunking() {
    let line = refusal_line("workspace is not running");
    for chunked in [false, true] {
        assert_eq!(
            head_of(line.as_bytes(), chunked).await,
            Head::Refused("workspace is not running".into())
        );
    }
    // No line end before the stream ends: what came is the reason.
    assert_eq!(
        head_of(b"puddle-refused: cut", true).await,
        Head::Refused("cut".into())
    );
    // Only the first line counts; the rest is hostile noise.
    assert_eq!(
        head_of(b"puddle-refused: one\r\n\x1b[31mtwo\n", false).await,
        Head::Refused("one".into())
    );
}

#[tokio::test]
async fn a_refusal_without_a_line_end_stops_at_the_limit() {
    let (mut client, mut server) = tokio::io::duplex(1 << 16);
    server.write_all(REFUSAL_PREFIX.as_bytes()).await.unwrap();
    // Keeps the stream open and never sends a newline.
    server.write_all(&vec![b'x'; LINE_LIMIT]).await.unwrap();
    let Head::Refused(reason) = read_head(&mut client).await.unwrap() else {
        panic!("not a refusal");
    };
    assert_eq!(reason.len(), REASON_LIMIT);
    drop(server);
}

/// A server that sends `first`, then `x` one byte per read for ever, and fails once more than
/// `cap` bytes were taken (so a reader that never stops ends the test instead of hanging it).
struct Endless {
    first: std::collections::VecDeque<u8>,
    taken: usize,
    cap: usize,
}

impl AsyncRead for Endless {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let outcome = self
            .cap
            .checked_sub(self.taken)
            .ok_or_else(|| std::io::Error::other("read past the cap"));
        self.taken += 1;
        let b = self.first.pop_front().unwrap_or(b'x');
        buf.put_slice(&[b]);
        std::task::Poll::Ready(outcome.map(|_| ()))
    }
}

#[tokio::test]
async fn a_refusal_without_a_line_end_is_read_up_to_exactly_the_limit() {
    // 16 bytes of prefix and 4 bytes per character of the longest reason, plus the CRLF:
    // written out, so a changed limit shows.
    let mut server = Endless {
        first: REFUSAL_PREFIX.bytes().collect(),
        taken: 0,
        cap: 100_000,
    };
    let head = read_head(&mut server).await.unwrap();
    assert_eq!(server.taken, 16 + 1024 * 4 + 2);
    assert_eq!(head, Head::Refused("x".repeat(1024)));
}

#[tokio::test]
async fn a_refusal_of_only_the_prefix_is_an_empty_reason() {
    // The stream ends right after the prefix: nothing more is read to decide it is a refusal.
    assert_eq!(
        head_of(REFUSAL_PREFIX.as_bytes(), false).await,
        Head::Refused(String::new())
    );
    assert_eq!(
        head_of(REFUSAL_PREFIX.as_bytes(), true).await,
        Head::Refused(String::new())
    );
}

#[test]
fn a_separating_space_never_makes_the_reason_longer_than_the_limit() {
    // The 1024th character would be a space with nothing after it.
    let reason = format!("{} b", "a".repeat(1023));
    assert_eq!(sanitize_reason(&reason), "a".repeat(1023));
    // One character fewer: the space and the next character both fit.
    let reason = format!("{} b", "a".repeat(1022));
    assert_eq!(sanitize_reason(&reason), format!("{} b", "a".repeat(1022)));
    assert_eq!(sanitize_reason(&reason).chars().count(), 1024);
}
