// SPDX-License-Identifier: AGPL-3.0-or-later
use futures_util::StreamExt;
use puddle_agent_proto::forward::ANSWER_TIMEOUT;
use puddle_agent_proto::host_control::PREAMBLE as HOST_CONTROL;
use puddle_test_kit::capture_logs;
use tokio::io::AsyncWriteExt;
use tokio::task::JoinHandle;
use tokio_yamux::{Config, Session, StreamHandle};

use super::*;

/// Opens a stream from a "host" session, writes `first` on it (nothing for an empty slice) and
/// hands the agent's end to [`HostStreams`]. Returns the host's end and the agent's serving task.
async fn serve(first: &[u8]) -> (StreamHandle, JoinHandle<()>) {
    let (a, b) = tokio::io::duplex(1 << 20);
    let mut host = Session::new_server(a, Config::default());
    let mut agent = Session::new_client(b, Config::default());
    let mut control = host.control();
    tokio::spawn(async move { while host.next().await.is_some() {} });
    let mut opened = control.open_stream().await.unwrap();
    if !first.is_empty() {
        opened.write_all(first).await.unwrap();
    }
    let inbound = agent.next().await.unwrap().unwrap();
    tokio::spawn(async move { while agent.next().await.is_some() {} });
    let streams = HostStreams::new(Forwards::new(vec![]), Vec::new(), None, None);
    (
        opened,
        tokio::spawn(async move { streams.serve(inbound).await }),
    )
}

#[tokio::test(start_paused = true)]
async fn a_stream_that_sends_no_preamble_in_time_is_dropped_and_logged() {
    let (log, _guard) = capture_logs();
    let (_host, serving) = serve(b"").await;
    tokio::time::timeout(ANSWER_TIMEOUT * 2, serving)
        .await
        .expect("the stream was left open")
        .unwrap();
    assert!(
        log.text()
            .contains("a stream from the host sent no preamble in time and was dropped"),
        "{}",
        log.text()
    );
}

#[tokio::test(start_paused = true)]
async fn a_host_control_stream_that_sends_no_message_in_time_is_dropped_and_logged() {
    let (log, _guard) = capture_logs();
    let (_host, serving) = serve(HOST_CONTROL).await;
    tokio::time::timeout(ANSWER_TIMEOUT * 2, serving)
        .await
        .expect("the stream was left open")
        .unwrap();
    assert!(
        log.text()
            .contains("a host-control stream sent no message in time and was dropped"),
        "{}",
        log.text()
    );
}
