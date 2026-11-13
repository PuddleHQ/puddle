// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use tokio::io::{AsyncReadExt as _, duplex};

#[tokio::test]
async fn the_watch_passes_bytes_through_and_notes_only_the_clients_end_of_stream() {
    let (client, server) = duplex(64);
    let (mut watched, closed) = ClientWatch::new(server);
    let (mut client_read, mut client_write) = tokio::io::split(client);

    client_write.write_all(b"ping").await.unwrap();
    let mut got = [0_u8; 4];
    watched.read_exact(&mut got).await.unwrap();
    assert_eq!(&got, b"ping");
    assert!(!closed.load(Ordering::Acquire), "data is not an end");

    watched.write_all(b"pong").await.unwrap();
    watched.flush().await.unwrap();
    let mut back = [0_u8; 4];
    client_read.read_exact(&mut back).await.unwrap();
    assert_eq!(&back, b"pong");

    // Our own shutdown is not the client leaving.
    watched.shutdown().await.unwrap();
    assert!(!closed.load(Ordering::Acquire));
    // A zero-length read into a full buffer is no end either.
    assert_eq!(watched.read(&mut []).await.unwrap(), 0);
    assert!(!closed.load(Ordering::Acquire));

    client_write.shutdown().await.unwrap();
    assert_eq!(watched.read(&mut got).await.unwrap(), 0);
    assert!(closed.load(Ordering::Acquire));
}

/// Counts the flushes and shutdowns that reach it.
#[derive(Default)]
struct Recording {
    flushes: usize,
    shutdowns: usize,
}

impl AsyncRead for Recording {
    fn poll_read(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        _buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

impl AsyncWrite for Recording {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.flushes += 1;
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.shutdowns += 1;
        Poll::Ready(Ok(()))
    }
}

#[tokio::test]
async fn the_watch_forwards_flush_and_shutdown_to_the_stream() {
    let (mut watched, _closed) = ClientWatch::new(Recording::default());
    watched.write_all(b"x").await.unwrap();
    assert_eq!(watched.read(&mut [0_u8; 1]).await.unwrap(), 0);
    watched.flush().await.unwrap();
    assert_eq!((watched.inner.flushes, watched.inner.shutdowns), (1, 0));
    watched.shutdown().await.unwrap();
    assert_eq!((watched.inner.flushes, watched.inner.shutdowns), (1, 1));
}

#[test]
fn accept_failures_are_reported_once_per_run_and_a_stop_always() {
    let error = IpcError::NoRuntime;
    let mut reports = Vec::new();
    let mut failing = false;
    accept_failed(|r| reports.push(r), &mut failing, &error);
    accept_failed(|r| reports.push(r), &mut failing, &error);
    failing = false;
    accept_failed(|r| reports.push(r), &mut failing, &error);
    accept_stopped(|r| reports.push(r), &error);
    assert_eq!(reports.len(), 3, "{reports:?}");
    assert!(reports[0].starts_with("the SSH endpoint could not accept a connection ("));
    assert!(reports[2].starts_with("the SSH endpoint stopped accepting connections ("));
    assert!(reports[2].ends_with("restart the workspace to get direct SSH back"));
}

#[test]
fn a_recovery_is_reported_once_and_only_after_a_reported_failure() {
    let mut reports = 0;
    let mut failing = false;
    accept_recovered(|| reports += 1, &mut failing);
    assert_eq!(reports, 0, "nothing failed, nothing to withdraw");
    failing = true;
    accept_recovered(|| reports += 1, &mut failing);
    accept_recovered(|| reports += 1, &mut failing);
    assert_eq!(reports, 1);
    assert!(!failing);
}

#[test]
fn starting_outside_a_runtime_is_refused() {
    struct Never;
    impl SshTarget for Never {
        type Error = io::Error;
        fn label(&self) -> String {
            "never".into()
        }
        fn admit(&self) -> impl Future<Output = Result<(), String>> {
            std::future::ready(Err("never".into()))
        }
        fn serve<S: SshStream>(&self, _stream: S) -> impl Future<Output = Result<(), Self::Error>> {
            std::future::ready(Ok(()))
        }
    }
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let root = puddle_ipc::IpcRoot::new().unwrap();
    let listener = rt.block_on(async { root.listen().unwrap() });
    let err = SshEndpoint::start(listener, Arc::new(Never)).unwrap_err();
    assert!(matches!(err, IpcError::NoRuntime), "{err}");
    // The helper target itself, so it is no dead code.
    assert_eq!(Never.label(), "never");
    // The reports default to doing nothing.
    Never.failed("x");
    Never.recovered();
    rt.block_on(async {
        assert!(Never.admit().await.is_err());
        Never.serve(duplex(1).0).await.unwrap();
    });
}
