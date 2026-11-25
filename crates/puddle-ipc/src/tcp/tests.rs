// SPDX-License-Identifier: AGPL-3.0-or-later

use tokio::io::AsyncWriteExt as _;
use tokio::sync::oneshot;

use super::*;

#[tokio::test]
async fn every_connection_reaches_the_handler_and_stop_returns_the_open_ones() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (seen_tx, mut seen) = tokio::sync::mpsc::unbounded_channel();
    let (stop_tx, stop_rx) = oneshot::channel::<()>();
    let (hold_tx, hold_rx) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(accept_tcp(
        listener,
        Duration::from_millis(1),
        "test",
        async move {
            let _ = stop_rx.await;
        },
        move |set, mut stream, _peer| {
            seen_tx.send(()).unwrap();
            let mut hold = hold_rx.clone();
            set.spawn(async move {
                let _ = hold.wait_for(|release| *release).await;
                let _ = stream.shutdown().await;
            });
        },
    ));
    let mut clients = Vec::new();
    for _ in 0..3 {
        clients.push(TcpStream::connect(addr).await.unwrap());
    }
    for _ in 0..3 {
        seen.recv().await.unwrap();
    }
    stop_tx.send(()).unwrap();
    let open = task.await.unwrap();
    assert_eq!(open.len(), 3);
    assert!(
        TcpStream::connect(addr).await.is_err(),
        "the listener is closed on return"
    );
    hold_tx.send(true).unwrap();
}
