// SPDX-License-Identifier: AGPL-3.0-or-later
use std::time::Duration;

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use super::*;

#[tokio::test]
async fn a_cut_ends_a_read_that_waits_and_the_writes_after_it() {
    let (near, mut far) = tokio::io::duplex(64);
    let cutoff = Arc::new(Cutoff::default());
    let mut stream = Cuttable::new(near, Arc::clone(&cutoff));
    far.write_all(b"ab").await.unwrap();
    let mut two = [0_u8; 2];
    stream.read_exact(&mut two).await.unwrap();
    assert_eq!(&two, b"ab");
    let waiting = tokio::spawn(async move {
        let mut more = [0_u8; 1];
        let read = stream.read(&mut more).await;
        (read, stream.write_all(b"x").await, stream.flush().await)
    });
    // flake-ok: lets the read start waiting; a cut before it starts is checked at its first poll too.
    tokio::time::sleep(Duration::from_millis(50)).await;
    cutoff.cut();
    let (read, write, flush) = tokio::time::timeout(Duration::from_secs(5), waiting)
        .await
        .expect("the cut wakes the read")
        .unwrap();
    assert_eq!(read.unwrap_err().kind(), io::ErrorKind::ConnectionAborted);
    assert_eq!(write.unwrap_err().kind(), io::ErrorKind::ConnectionAborted);
    assert_eq!(flush.unwrap_err().kind(), io::ErrorKind::ConnectionAborted);
}

#[tokio::test]
async fn closing_the_stream_reaches_the_stream_under_it() {
    let (near, mut far) = tokio::io::duplex(64);
    let mut stream = Cuttable::new(near, Arc::new(Cutoff::default()));
    stream.write_all(b"x").await.unwrap();
    stream.shutdown().await.unwrap();
    let mut seen = Vec::new();
    far.read_to_end(&mut seen).await.unwrap();
    assert_eq!(seen, b"x");
}

#[tokio::test]
async fn dropping_the_guard_cuts() {
    let cutoff = Arc::new(Cutoff::default());
    let guard = CutOnDrop(Arc::clone(&cutoff));
    assert!(!cutoff.is_cut());
    drop(guard);
    assert!(cutoff.is_cut());
}
