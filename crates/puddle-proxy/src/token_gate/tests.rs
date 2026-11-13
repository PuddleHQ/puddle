// SPDX-License-Identifier: AGPL-3.0-or-later
use puddle_ca::CaBuilder;
use puddle_types::Host;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::*;
use crate::terminate::{
    Exchange, ExchangeRewriter, NoInjection, TerminationSet, test_client_hello,
};

#[derive(Debug)]
struct Endpoints;

impl ExchangeRewriter for Endpoints {
    fn begin(&self, _: &Host, _: &str, _: &str) -> Option<Exchange> {
        None
    }

    fn is_token_endpoint(&self, host: &Host) -> bool {
        *host == Host::parse_normalised("tokens.test").unwrap()
    }
}

fn termination(with_exchanges: bool) -> Arc<Termination> {
    let ca = Arc::new(CaBuilder::new("test").build().unwrap());
    let termination = Termination::new(TerminationSet::new(), ca, Arc::new(NoInjection));
    let termination = if with_exchanges {
        termination.with_exchanges(Arc::new(Endpoints))
    } else {
        termination
    };
    Arc::new(termination)
}

/// Writes `chunks` from the guest side and reads everything the gate passes on.
async fn through(
    termination: Option<Arc<Termination>>,
    chunks: &[&[u8]],
) -> (Vec<u8>, bool, Option<io::ErrorKind>) {
    let (mut guest, tunnel) = tokio::io::duplex(64 * 1024);
    let mut gate = TokenGate::new(tunnel, termination, &[]);
    let mut passed = Vec::new();
    let mut failure = None;
    for chunk in chunks {
        guest.write_all(chunk).await.unwrap();
        let mut buf = vec![0_u8; 64 * 1024];
        match gate.read(&mut buf).await {
            Ok(n) => passed.extend_from_slice(&buf[..n]),
            Err(err) => {
                failure = Some(err.kind());
                break;
            }
        }
    }
    (passed, gate.refused(), failure)
}

#[tokio::test]
async fn a_hello_for_a_token_endpoint_is_dropped_and_ends_the_read() {
    let hello = test_client_hello("tokens.test", &[]);
    let (passed, refused, failure) = through(Some(termination(true)), &[&hello]).await;
    assert!(passed.is_empty(), "nothing of the hello is passed on");
    assert!(refused);
    assert_eq!(failure, Some(io::ErrorKind::PermissionDenied));
}

#[tokio::test]
async fn the_name_is_read_whatever_its_case() {
    let hello = test_client_hello("TOKENS.test", &[]);
    let (_, refused, _) = through(Some(termination(true)), &[&hello]).await;
    assert!(refused);
}

#[tokio::test]
async fn another_name_passes_unchanged_and_later_uploads_are_not_read() {
    let hello = test_client_hello("other.test", &[]);
    let later = test_client_hello("tokens.test", &[]);
    let (passed, refused, failure) = through(Some(termination(true)), &[&hello, &later]).await;
    assert!(!refused && failure.is_none());
    assert_eq!([hello, later].concat(), passed);
}

#[tokio::test]
async fn without_exchanges_or_a_termination_nothing_is_refused() {
    let hello = test_client_hello("tokens.test", &[]);
    for termination in [Some(termination(false)), None] {
        let (passed, refused, failure) = through(termination, &[&hello]).await;
        assert!(!refused && failure.is_none());
        assert_eq!(hello, passed);
    }
}

#[tokio::test]
async fn another_protocol_passes_and_is_not_read_again() {
    let hello = test_client_hello("tokens.test", &[]);
    let (passed, refused, _) = through(Some(termination(true)), &[b"hello there", &hello]).await;
    assert!(!refused);
    assert_eq!([&b"hello there"[..], &hello].concat(), passed);
}

#[tokio::test]
async fn a_hello_split_over_reads_is_refused_when_its_last_part_arrives() {
    let hello = test_client_hello("tokens.test", &[]);
    let (first, rest) = hello.split_at(7);
    let (passed, refused, failure) = through(Some(termination(true)), &[first, rest]).await;
    assert_eq!(first, passed, "the first part went on, the last never does");
    assert!(refused);
    assert_eq!(failure, Some(io::ErrorKind::PermissionDenied));
}

#[tokio::test]
async fn a_hello_that_never_ends_is_given_up_on_after_the_cap() {
    let mut long = vec![0x16, 3, 3, 0x3f, 0xff, 1];
    long.resize(MAX_SEEN + 1, 0);
    let (passed, refused, _) = through(Some(termination(true)), &[&long]).await;
    assert!(!refused);
    assert_eq!(long, passed);
}

#[test]
fn a_record_that_is_still_open_is_watched_up_to_the_cap_and_no_further() {
    let (_guest, tunnel) = tokio::io::duplex(64);
    let mut gate = TokenGate::new(tunnel, Some(termination(true)), &[]);
    // A handshake record that claims 16383 bytes and never ends.
    let mut open = vec![0x16, 3, 3, 0x3f, 0xff, 1];
    open.resize(MAX_SEEN - 1, 0);
    gate.look(&open);
    assert!(
        gate.termination.is_some(),
        "one byte under the cap: watched"
    );
    gate.look(&[0]);
    assert!(gate.termination.is_none(), "at the cap: given up");
    assert_eq!(MAX_SEEN, 16 * 1024);
}

/// A stream that counts the flushes it is asked for.
#[derive(Default)]
struct Flushes(usize);

impl AsyncRead for Flushes {
    fn poll_read(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
        _: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

impl AsyncWrite for Flushes {
    fn poll_write(self: Pin<&mut Self>, _: &mut Context<'_>, b: &[u8]) -> Poll<io::Result<usize>> {
        Poll::Ready(Ok(b.len()))
    }

    fn poll_flush(mut self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.0 += 1;
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

#[tokio::test]
async fn a_flush_reaches_the_stream_under_the_gate() {
    let mut gate = TokenGate::new(Flushes::default(), None, &[]);
    gate.flush().await.unwrap();
    assert_eq!(gate.inner().0, 1);
    gate.write_all(b"x").await.unwrap();
    gate.shutdown().await.unwrap();
    assert_eq!(gate.read(&mut [0_u8; 1]).await.unwrap(), 0);
}

#[tokio::test]
async fn once_refused_every_read_fails() {
    let (_guest, tunnel) = tokio::io::duplex(64);
    let hello = test_client_hello("tokens.test", &[]);
    let mut gate = TokenGate::new(tunnel, Some(termination(true)), &hello);
    let host = Host::parse_normalised("tokens.test").unwrap();
    assert!(Endpoints.begin(&host, "POST", "/").is_none());
    let err = gate.read(&mut [0_u8; 8]).await.unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
}

#[tokio::test]
async fn a_hello_sent_with_the_request_is_checked_at_once() {
    let (_guest, tunnel) = tokio::io::duplex(64);
    let hello = test_client_hello("tokens.test", &[]);
    assert!(TokenGate::new(tunnel, Some(termination(true)), &hello).refused());
    let (_guest, tunnel) = tokio::io::duplex(64);
    let other = test_client_hello("other.test", &[]);
    assert!(!TokenGate::new(tunnel, Some(termination(true)), &other).refused());
}

#[tokio::test]
async fn writes_go_through_untouched() {
    let (mut guest, tunnel) = tokio::io::duplex(64);
    let mut gate = TokenGate::new(tunnel, Some(termination(true)), &[]);
    gate.write_all(b"answer").await.unwrap();
    gate.flush().await.unwrap();
    let mut got = [0_u8; 6];
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        guest.read_exact(&mut got),
    )
    .await
    .expect("the bytes arrive")
    .unwrap();
    assert_eq!(&got, b"answer");
}
