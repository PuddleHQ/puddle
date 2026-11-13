// SPDX-License-Identifier: AGPL-3.0-or-later
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::*;

#[tokio::test]
async fn counts_both_directions_and_outlive_the_stream() {
    let (near, mut far) = tokio::io::duplex(64);
    let (mut stream, counts) = Counted::new(near);
    far.write_all(b"hello").await.unwrap();
    let mut buf = [0; 5];
    stream.read_exact(&mut buf).await.unwrap();
    stream.write_all(b"abc").await.unwrap();
    stream.flush().await.unwrap();
    stream.shutdown().await.unwrap();
    drop(stream);
    let mut back = Vec::new();
    far.read_to_end(&mut back).await.unwrap();
    assert_eq!(back, b"abc");
    assert_eq!((counts.read(), counts.written()), (5, 3));
}

#[tokio::test]
async fn a_flush_reaches_the_stream_underneath() {
    let (near, mut far) = tokio::io::duplex(64);
    let (mut stream, _) = Counted::new(tokio::io::BufWriter::new(near));
    stream.write_all(b"abc").await.unwrap();
    stream.flush().await.unwrap();
    let mut got = [0; 3];
    tokio::time::timeout(std::time::Duration::from_secs(5), far.read_exact(&mut got))
        .await
        .expect("the flush pushed the buffered bytes out")
        .unwrap();
    assert_eq!(&got, b"abc");
}
