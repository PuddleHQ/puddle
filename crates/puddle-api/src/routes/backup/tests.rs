// SPDX-License-Identifier: AGPL-3.0-or-later
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures_util::StreamExt as _;
use tokio::io::{AsyncRead, ReadBuf};

use super::*;

/// Yields `first`, then fails.
struct FailsAfter {
    first: Option<Vec<u8>>,
}

impl AsyncRead for FailsAfter {
    fn poll_read(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.first.take() {
            Some(bytes) => {
                buf.put_slice(&bytes);
                Poll::Ready(Ok(()))
            }
            None => Poll::Ready(Err(io::Error::other("the disk went away"))),
        }
    }
}

#[tokio::test]
async fn a_file_larger_than_a_piece_is_sent_in_pieces_and_ends() {
    let bytes: Vec<u8> = (0..CHUNK_BYTES * 2 + 10)
        .map(|i| u8::try_from(i % 251).unwrap())
        .collect();
    let pieces: Vec<_> = chunks(Box::pin(io::Cursor::new(bytes.clone())))
        .collect()
        .await;
    let sizes: Vec<_> = pieces.iter().map(|p| p.as_ref().unwrap().len()).collect();
    assert_eq!(sizes, [CHUNK_BYTES, CHUNK_BYTES, 10]);
    let joined: Vec<u8> = pieces.into_iter().flat_map(Result::unwrap).collect();
    assert_eq!(joined, bytes);
}

#[tokio::test]
async fn a_read_error_is_the_last_item_so_the_client_sees_a_failed_download() {
    let reader = FailsAfter {
        first: Some(b"some bytes".to_vec()),
    };
    let items: Vec<_> = chunks(Box::pin(reader)).collect().await;
    assert_eq!(items.len(), 2, "{items:?}");
    assert_eq!(items[0].as_ref().unwrap(), b"some bytes");
    assert!(items[1].is_err());
}
