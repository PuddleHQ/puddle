// SPDX-License-Identifier: AGPL-3.0-or-later
use puddle_ipc::IpcRoot;
use tokio::io::AsyncWriteExt;

use super::*;

async fn send(path: &std::path::Path, bytes: &[u8]) {
    let mut conn = puddle_ipc::connect(path).await.unwrap();
    conn.write_all(bytes).await.unwrap();
    conn.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_sender_that_connects_after_a_long_idle_is_still_collected() {
    let root = IpcRoot::new().unwrap();
    let listener = root.listen().unwrap();
    let path = listener.endpoint().path().to_owned();
    let collector = collect_lines_within(listener, Duration::from_secs(5));
    // A stalled boot sends late; only the test's signal ends the collection.
    tokio::time::sleep(Duration::from_millis(400)).await;
    send(&path, b"late\n").await;
    send(&path, b"later\n").await;
    assert_eq!(collector.finish().await.unwrap(), ["late", "later"]);
}

#[tokio::test]
async fn the_stop_signal_ends_a_collector_that_saw_nothing() {
    let root = IpcRoot::new().unwrap();
    let collector = collect_lines(root.listen().unwrap());
    let lines = tokio::time::timeout(Duration::from_secs(5), collector.finish())
        .await
        .expect("finish returns on the signal, not on an idle bound");
    assert_eq!(lines.unwrap(), Vec::<String>::new());
}

#[tokio::test]
async fn a_sender_that_stalls_is_reported_as_a_read_timeout() {
    let root = IpcRoot::new().unwrap();
    let listener = root.listen().unwrap();
    let path = listener.endpoint().path().to_owned();
    let collector = collect_lines_within(listener, Duration::from_millis(200));
    let mut conn = puddle_ipc::connect(&path).await.unwrap();
    conn.write_all(b"half").await.unwrap();
    let err = collector.finish().await.unwrap_err();
    assert!(
        err.contains("read timed out") && err.contains("half"),
        "{err}"
    );
    drop(conn);
}
