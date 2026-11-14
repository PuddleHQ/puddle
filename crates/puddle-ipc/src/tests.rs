// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn client_errors_map_to_specific_variants() {
    let p = Path::new("/x");
    assert!(matches!(
        client_error(p, io::Error::from(io::ErrorKind::NotFound)),
        IpcError::NotFound { .. }
    ));
    assert!(matches!(
        client_error(p, io::Error::from(io::ErrorKind::PermissionDenied)),
        IpcError::AccessDenied { .. }
    ));
    assert!(matches!(
        client_error(p, io::Error::from(io::ErrorKind::BrokenPipe)),
        IpcError::Io {
            op: "connect to",
            ..
        }
    ));
}

#[test]
fn binding_outside_a_runtime_is_refused() {
    let root = IpcRoot::new().unwrap();
    let err = root.listen().unwrap_err();
    assert!(matches!(err, IpcError::NoRuntime), "{err}");
}

#[test]
fn connecting_outside_a_runtime_is_refused() {
    // Polled with no tokio context at all: the guard must answer before any I/O.
    // flake-ok: a path that must not exist; on Windows it is rooted on the current drive and still does not exist
    let err = block_on_without_runtime(connect(Path::new("/nonexistent"))).unwrap_err();
    assert!(matches!(err, IpcError::NoRuntime), "{err}");
}

/// Polls a future to completion without any tokio context.
fn block_on_without_runtime<F: Future>(fut: F) -> F::Output {
    let mut cx = Context::from_waker(std::task::Waker::noop());
    let mut fut = std::pin::pin!(fut);
    loop {
        if let Poll::Ready(v) = fut.as_mut().poll(&mut cx) {
            return v;
        }
    }
}

#[test]
fn endpoint_displays_its_path() {
    let e = Endpoint {
        path: PathBuf::from("/a/b.sock"),
    };
    assert_eq!(e.to_string(), "/a/b.sock");
    assert_eq!(e.path(), Path::new("/a/b.sock"));
}
