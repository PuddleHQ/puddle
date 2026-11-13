// SPDX-License-Identifier: AGPL-3.0-or-later
use futures_util::StreamExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_yamux::{Config, Session};

use super::*;
use crate::host_streams::HostStreams;

/// Opens a stream from a "host" session, writes `first` on it, and hands the agent's end to
/// [`HostStreams`] as the agent would. Returns the host's end and what the agent's side took.
async fn serve(
    forwards: Arc<Forwards>,
    first: &[u8],
) -> (StreamHandle, tokio::task::JoinHandle<()>) {
    let (a, b) = tokio::io::duplex(1 << 20);
    let mut host = Session::new_server(a, Config::default());
    let mut agent = Session::new_client(b, Config::default());
    let mut control = host.control();
    tokio::spawn(async move { while host.next().await.is_some() {} });
    let mut opened = control.open_stream().await.unwrap();
    opened.write_all(first).await.unwrap();
    let serving = tokio::spawn(async move {
        let inbound = agent.next().await.unwrap().unwrap();
        let streams = HostStreams::new(forwards, Vec::new(), None, None);
        let task = tokio::spawn(async move { streams.serve(inbound).await });
        while agent.next().await.is_some() {}
        task.await.unwrap();
    });
    (opened, serving)
}

#[tokio::test]
async fn a_stream_that_is_not_a_forward_request_is_dropped_without_an_answer() {
    for first in [
        &b"GET / HTTP/1.1\r\n\r\n"[..],
        b"\0puddle-forward/2\n80\n",
        b"\0puddle-control/1\n80\n",
        b"\0puddle-forward/1\nnot-a-port\n",
        b"\0puddle-forward/1\n0\n",
    ] {
        let (mut host, _serving) = serve(Forwards::new(vec![]), first).await;
        let mut buf = Vec::new();
        let read = tokio::time::timeout(Duration::from_secs(5), host.read_to_end(&mut buf)).await;
        assert!(read.is_ok(), "{first:?}: the stream was left open");
        assert!(buf.is_empty(), "{first:?}: answered {buf:?}");
    }
}

#[tokio::test]
async fn a_stream_closed_before_its_request_is_dropped() {
    let (mut host, _serving) = serve(Forwards::new(vec![]), b"\0puddle-forward/1\n80").await;
    host.shutdown().await.unwrap();
    let mut buf = Vec::new();
    let read = tokio::time::timeout(Duration::from_secs(5), host.read_to_end(&mut buf)).await;
    assert!(read.is_ok(), "the stream was left open");
    assert!(buf.is_empty(), "answered {buf:?}");
}

#[tokio::test(start_paused = true)]
async fn a_stream_that_sends_no_request_in_time_is_dropped() {
    let (mut host, _serving) = serve(Forwards::new(vec![]), b"\0puddle-forward/1\n").await;
    let mut buf = Vec::new();
    let read = tokio::time::timeout(ANSWER_TIMEOUT * 2, host.read_to_end(&mut buf)).await;
    assert!(read.is_ok(), "the stream was left open");
    assert!(buf.is_empty(), "answered {buf:?}");
}

#[tokio::test]
async fn past_the_limit_a_forward_is_refused_with_a_reason() {
    let forwards = Forwards::new(vec![]);
    let held = forwards
        .open
        .acquire_many(u32::try_from(MAX_FORWARDS).unwrap())
        .await
        .unwrap();
    let (mut host, _serving) = serve(Arc::clone(&forwards), b"\0puddle-forward/1\n8080\n").await;
    let mut answer = String::new();
    host.read_to_string(&mut answer).await.unwrap();
    assert!(answer.starts_with("refused too many"), "{answer}");
    drop(held);
}

#[test]
fn a_refusal_means_nothing_listens_and_any_other_failure_is_named() {
    assert_eq!(
        failure_text(io::ErrorKind::ConnectionRefused, 80),
        "nothing in the workspace listens on port 80"
    );
    let other = failure_text(io::ErrorKind::TimedOut, 80);
    assert!(
        other.contains("did not take a connection on port 80") && other.contains("timed out"),
        "{other}"
    );
}
