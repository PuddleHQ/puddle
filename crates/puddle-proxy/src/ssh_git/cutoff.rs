// SPDX-License-Identifier: GPL-3.0-or-later
//! A stream that can be cut from outside.
//!
//! russh runs a connection in a task of its own and answers a `disconnect` only between key
//! exchanges, so a guest that sends its identification line and then nothing stays in the key
//! exchange for ever: the handshake limit could only drop the handle, never the connection. A
//! [`Cutoff`] ends the stream's reads and writes with an error, which ends the task and drops the
//! stream under it.

use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll, Waker};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

/// The switch of one stream; [`Cutoff::cut`] ends it.
#[derive(Debug, Default)]
pub(crate) struct Cutoff {
    cut: AtomicBool,
    reader: Mutex<Option<Waker>>,
}

impl Cutoff {
    /// Ends the stream: its next read or write fails, and a read that waits is woken to fail.
    pub(crate) fn cut(&self) {
        self.cut.store(true, Ordering::SeqCst);
        let waker = self
            .reader
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    fn is_cut(&self) -> bool {
        self.cut.load(Ordering::SeqCst)
    }
}

/// Cuts its [`Cutoff`] when dropped, so the end of a function ends the connection whichever way it
/// ends.
pub(crate) struct CutOnDrop(pub(crate) Arc<Cutoff>);

impl Drop for CutOnDrop {
    fn drop(&mut self) {
        self.0.cut();
    }
}

/// `inner`, cut by `cutoff`.
#[derive(Debug)]
pub(crate) struct Cuttable<S> {
    inner: S,
    cutoff: Arc<Cutoff>,
}

impl<S> Cuttable<S> {
    pub(crate) fn new(inner: S, cutoff: Arc<Cutoff>) -> Self {
        Self { inner, cutoff }
    }
}

fn cut_error() -> io::Error {
    io::Error::new(io::ErrorKind::ConnectionAborted, "the connection was cut")
}

impl<S: AsyncRead + Unpin> AsyncRead for Cuttable<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        // The waker goes in before the check, so a cut between the two still wakes this read.
        *self
            .cutoff
            .reader
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(cx.waker().clone());
        if self.cutoff.is_cut() {
            return Poll::Ready(Err(cut_error()));
        }
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Cuttable<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<io::Result<usize>> {
        if self.cutoff.is_cut() {
            return Poll::Ready(Err(cut_error()));
        }
        Pin::new(&mut self.inner).poll_write(cx, data)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        if self.cutoff.is_cut() {
            return Poll::Ready(Err(cut_error()));
        }
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
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
}
