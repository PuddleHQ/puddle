// SPDX-License-Identifier: GPL-3.0-or-later
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::*;

fn head(headers: &[&str]) -> Head {
    Head {
        method: "CONNECT".into(),
        uri: "github.com:22".into(),
        version: "HTTP/1.1".into(),
        headers: headers.iter().map(|h| (*h).to_owned()).collect(),
    }
}

#[test]
fn only_the_origin_header_with_the_value_container_marks_a_container() {
    assert!(from_container(&head(&["x-puddle-origin: container"])));
    assert!(from_container(&head(&[
        "Host: a",
        "X-Puddle-Origin:  Container "
    ])));
    for headers in [
        &[][..],
        &["x-puddle-origin: guest"],
        &["x-puddle-origin: "],
        &["x-puddle-origin-extra: container"],
        &["x-puddle-protocol: container"],
        &["host: x-puddle-origin: container"],
    ] {
        assert!(!from_container(&head(headers)), "{headers:?}");
    }
}

#[test]
fn only_the_protocol_header_with_the_value_ssh_announces_it() {
    assert!(announced(&head(&["x-puddle-protocol: ssh"])));
    assert!(announced(&head(&["Host: a", "X-Puddle-Protocol:  SSH "])));
    for headers in [
        &[][..],
        &["x-puddle-protocol: ssh2"],
        &["x-puddle-protocol: "],
        &["x-puddle-protocol-extra: ssh"],
        &["x-other: ssh"],
        &["host: x-puddle-protocol: ssh"],
    ] {
        assert!(!announced(&head(headers)), "{headers:?}");
    }
}

#[test]
fn bytes_behind_the_head_start_ssh_only_with_an_identification_line() {
    assert!(started(b"SSH-2.0-OpenSSH_10.0\r\n"));
    assert!(!started(b""));
    assert!(!started(b"\x16\x03\x01"));
    assert!(!started(b"GET / HTTP/1.1\r\n"));
}

#[test]
fn the_pre_banner_line_is_one_line_that_is_not_a_banner() {
    let line = pre_banner("SSH is not supported yet");
    assert_eq!(line, b"puddle: SSH is not supported yet\r\n");
    assert!(!is_banner(&line));
}

#[tokio::test]
async fn a_first_chunk_that_is_a_banner_fails_the_read_and_drops_the_bytes() {
    let (near, mut far) = tokio::io::duplex(256);
    let mut gate = Gate::new(near, Vec::new(), true);
    far.write_all(b"SSH-2.0-test\r\n").await.unwrap();
    let mut buf = [0u8; 64];
    let err = gate.read(&mut buf).await.unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
    assert!(gate.ssh_seen());
    // The line was taken from the stream and not handed on.
    far.shutdown().await.unwrap();
    assert_eq!(gate.read(&mut buf).await.unwrap(), 0);
}

#[tokio::test]
async fn any_other_first_chunk_and_every_later_one_pass_unchanged() {
    let (near, mut far) = tokio::io::duplex(256);
    let mut gate = Gate::new(near, Vec::new(), true);
    far.write_all(b"hello").await.unwrap();
    let mut buf = [0u8; 64];
    assert_eq!(gate.read(&mut buf).await.unwrap(), 5);
    far.write_all(b"SSH-2.0-later\r\n").await.unwrap();
    let n = gate.read(&mut buf).await.unwrap();
    assert_eq!(buf.get(..n), Some(&b"SSH-2.0-later\r\n"[..]));
    assert!(!gate.ssh_seen());
}

#[tokio::test]
async fn a_gate_that_is_not_watching_passes_a_banner_and_writes_go_through() {
    let (near, mut far) = tokio::io::duplex(256);
    let mut gate = Gate::new(near, Vec::new(), false);
    far.write_all(b"SSH-2.0-test\r\n").await.unwrap();
    let mut buf = [0u8; 64];
    let n = gate.read(&mut buf).await.unwrap();
    assert_eq!(buf.get(..n), Some(&b"SSH-2.0-test\r\n"[..]));
    gate.write_all(b"down").await.unwrap();
    gate.flush().await.unwrap();
    gate.shutdown().await.unwrap();
    let mut back = Vec::new();
    far.read_to_end(&mut back).await.unwrap();
    assert_eq!(back, b"down");
    assert!(!gate.ssh_seen());
}

/// A writer that notes that it was flushed and shut down (a duplex stream does neither visibly).
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
async fn a_flush_and_a_shutdown_reach_the_stream_under_the_gate() {
    let mut gate = Gate::new(Sink::default(), Vec::new(), false);
    gate.write_all(b"x").await.unwrap();
    gate.flush().await.unwrap();
    assert!(gate.inner.flushed && !gate.inner.shut);
    gate.shutdown().await.unwrap();
    assert!(gate.inner.shut);
}

#[tokio::test]
async fn the_end_of_the_stream_before_any_byte_is_not_ssh() {
    let (near, mut far) = tokio::io::duplex(256);
    let mut gate = Gate::new(near, Vec::new(), true);
    far.shutdown().await.unwrap();
    let mut buf = [0u8; 8];
    assert_eq!(gate.read(&mut buf).await.unwrap(), 0);
    assert!(!gate.ssh_seen());
}

/// A reader that first reads nothing, then yields what it was given.
struct EmptyThen(Option<&'static [u8]>, bool);

impl AsyncRead for EmptyThen {
    fn poll_read(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if self.1 {
            if let Some(data) = self.0.take() {
                buf.put_slice(data);
            }
        } else {
            self.1 = true;
        }
        Poll::Ready(Ok(()))
    }
}

#[tokio::test]
async fn a_read_that_brings_no_bytes_does_not_end_the_watch_for_the_banner() {
    let mut gate = Gate::new(EmptyThen(Some(b"SSH-2.0-x\r\n"), false), Vec::new(), true);
    let mut buf = [0_u8; 32];
    assert_eq!(gate.read(&mut buf).await.unwrap(), 0);
    let err = gate.read(&mut buf).await.unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
    assert!(gate.ssh_seen());
}

#[test]
fn the_start_of_an_upload_is_undecided_only_while_it_could_still_become_a_banner() {
    for first in [&b""[..], b"S", b"SS", b"SSH", b"SSH-"] {
        assert!(undecided(first), "{first:?}");
    }
    for first in [
        &b"SSH-2"[..],
        b"SSH-x",
        b"SSL",
        b"s",
        b"\x16",
        b"SSH-2.0-x\r\n",
        b"GET ",
    ] {
        assert!(!undecided(first), "{first:?}");
    }
}

#[tokio::test]
async fn a_banner_split_over_several_reads_is_still_refused() {
    let (near, mut far) = tokio::io::duplex(256);
    let mut gate = Gate::new(near, Vec::new(), true);
    let mut buf = [0u8; 64];
    let read = tokio::spawn(async move {
        let err = gate.read(&mut buf).await.unwrap_err();
        (err.kind(), gate.ssh_seen())
    });
    // flake-ok: a send loop, not a wait for a condition: it writes each chunk of a split banner.
    for chunk in [&b"S"[..], b"SH", b"-", b"2.0-x\r\n"] {
        far.write_all(chunk).await.unwrap();
        tokio::task::yield_now().await;
    }
    assert_eq!(read.await.unwrap(), (io::ErrorKind::PermissionDenied, true));
}

#[tokio::test]
async fn a_banner_begun_with_the_request_and_ended_after_it_is_still_refused() {
    let (near, mut far) = tokio::io::duplex(256);
    let mut gate = Gate::new(near, b"SS".to_vec(), true);
    far.write_all(b"H-2.0-x\r\n").await.unwrap();
    let mut buf = [0u8; 64];
    let err = gate.read(&mut buf).await.unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
    assert!(gate.ssh_seen());
}

#[tokio::test]
async fn bytes_sent_with_the_request_come_first_and_whole() {
    let (near, mut far) = tokio::io::duplex(256);
    let mut gate = Gate::new(near, b"\x16\x03\x01".to_vec(), true);
    far.write_all(b"rest").await.unwrap();
    far.shutdown().await.unwrap();
    let mut got = Vec::new();
    gate.read_to_end(&mut got).await.unwrap();
    assert_eq!(got, b"\x16\x03\x01rest");
    assert!(!gate.ssh_seen());
}

#[tokio::test]
async fn a_prefix_of_a_banner_that_turns_out_otherwise_or_ends_is_handed_on_whole() {
    for (early, later) in [
        (&b"SS"[..], &b"H-x more"[..]),
        (b"", b"SSH-"),
        (b"SSH-", b""),
    ] {
        let (near, mut far) = tokio::io::duplex(256);
        let mut gate = Gate::new(near, early.to_vec(), true);
        far.write_all(later).await.unwrap();
        far.shutdown().await.unwrap();
        let mut got = Vec::new();
        gate.read_to_end(&mut got).await.unwrap();
        assert_eq!(got, [early, later].concat(), "{early:?} {later:?}");
        assert!(!gate.ssh_seen());
    }
}

#[tokio::test]
async fn a_gate_that_is_not_watching_hands_on_what_came_with_the_request_first() {
    let (near, mut far) = tokio::io::duplex(256);
    let mut gate = Gate::new(near, b"SSH-2.0-early\r\n".to_vec(), false);
    far.write_all(b"then").await.unwrap();
    far.shutdown().await.unwrap();
    let mut got = Vec::new();
    gate.read_to_end(&mut got).await.unwrap();
    assert_eq!(got, b"SSH-2.0-early\r\nthen");
}

#[tokio::test]
async fn the_bytes_sent_with_the_head_come_out_whole_through_small_reads() {
    let (near, mut far) = tokio::io::duplex(256);
    let mut gate = Gate::new(near, b"GET /early".to_vec(), true);
    far.write_all(b" and later").await.unwrap();
    drop(far);
    let mut out = Vec::new();
    let mut buf = [0u8; 3];
    loop {
        let n = gate.read(&mut buf).await.unwrap();
        if n == 0 {
            break;
        }
        out.extend_from_slice(&buf[..n]);
    }
    assert_eq!(out, b"GET /early and later");
}
