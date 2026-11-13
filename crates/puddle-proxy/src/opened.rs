// SPDX-License-Identifier: GPL-3.0-or-later
//! A guest connection whose `CONNECT` head was read, on its way to a splice or a termination.

use std::io;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};

use crate::ssh;

/// The answer that opens a tunnel.
pub(crate) const ESTABLISHED: &[u8] = b"HTTP/1.1 200 Connection Established\r\n\r\n";

/// The guest's stream after its `CONNECT` head, the bytes it sent behind the head, and whether it
/// was answered already.
#[derive(Debug)]
pub(crate) struct Opened<S> {
    pub(crate) stream: S,
    /// Upload bytes taken from the stream but not passed on yet.
    pub(crate) early: Vec<u8>,
    /// Whether the guest was sent [`ESTABLISHED`].
    pub(crate) answered: bool,
}

impl<S: AsyncRead> From<BufReader<S>> for Opened<S> {
    /// The stream under `reader`, which just read the head, with what it buffered behind it.
    fn from(reader: BufReader<S>) -> Self {
        Self {
            early: reader.buffer().to_vec(),
            stream: reader.into_inner(),
            answered: false,
        }
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> Opened<S> {
    /// Sends [`ESTABLISHED`] unless it was sent already.
    pub(crate) async fn answer(&mut self) -> io::Result<()> {
        if !self.answered {
            self.stream.write_all(ESTABLISHED).await?;
            self.answered = true;
        }
        Ok(())
    }

    /// Reads on until the first upload bytes tell whether they are an SSH identification line
    /// ([`ssh::undecided`]), for at most `limit`. When what came with the head does not tell, the
    /// guest is answered first: a client may wait for the `200` before it sends anything. A stream
    /// that fails ends the read; whatever handles the connection next meets the same failure.
    pub(crate) async fn read_first(&mut self, limit: Duration) {
        if !ssh::undecided(&self.early) || self.answer().await.is_err() {
            return;
        }
        let read = async {
            let mut chunk = [0u8; 256];
            while ssh::undecided(&self.early) {
                match self.stream.read(&mut chunk).await {
                    Ok(n) if n > 0 => self
                        .early
                        .extend_from_slice(chunk.get(..n).unwrap_or_default()),
                    _ => break,
                }
            }
        };
        // On timeout, decide from what arrived: too little for a banner is decrypted.
        let _ = tokio::time::timeout(limit, read).await;
    }
}

#[cfg(test)]
mod tests;
