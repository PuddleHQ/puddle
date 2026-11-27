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

#[test]
fn the_install_id_is_made_once_and_kept() {
    let state = tempfile::tempdir().unwrap();
    let first = install_id(state.path()).unwrap();
    assert!(name::is_install_id(&first), "{first}");
    assert_eq!(install_id(state.path()).unwrap(), first);
    let other = tempfile::tempdir().unwrap();
    assert_ne!(install_id(other.path()).unwrap(), first);
}

#[test]
fn an_install_id_file_that_is_not_an_id_is_refused_and_left_alone() {
    // Remaking the id renames every route, which makes every sandbox again and drops its root
    // disk: only a missing file may do that.
    let state = tempfile::tempdir().unwrap();
    let file = state.path().join(INSTALL_ID_FILE);
    std::fs::write(&file, "not an id").unwrap();
    match install_id(state.path()).unwrap_err() {
        IpcError::Root { path, .. } => assert_eq!(path, file),
        other => panic!("unexpected {other}"),
    }
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "not an id");
}

#[test]
fn an_install_id_file_that_cannot_be_read_is_refused() {
    // A folder in its place reads as an error that is not "not found".
    let state = tempfile::tempdir().unwrap();
    let file = state.path().join(INSTALL_ID_FILE);
    std::fs::create_dir(&file).unwrap();
    let err = install_id(state.path()).unwrap_err();
    assert!(matches!(err, IpcError::Root { .. }), "{err}");
    assert!(file.is_dir());
}

#[test]
fn an_install_id_that_cannot_be_written_names_the_file() {
    let state = tempfile::tempdir().unwrap();
    let missing = state.path().join("a-file");
    std::fs::write(&missing, "x").unwrap();
    match install_id(&missing).unwrap_err() {
        IpcError::Root { path, .. } => assert_eq!(path, missing.join(INSTALL_ID_FILE)),
        other => panic!("unexpected {other}"),
    }
}

#[test]
fn endpoint_names_are_workspace_shaped_labels() {
    for ok in ["a", "acme-api", "a0000000", &"a".repeat(63)] {
        name::check_name(ok).unwrap();
    }
    for bad in [
        "",
        "Acme",
        "a/b",
        "a.b",
        "a b",
        "../x",
        "a_b",
        &"a".repeat(64),
    ] {
        let err = name::check_name(bad).unwrap_err();
        assert!(matches!(err, IpcError::InvalidName { .. }), "{bad}: {err}");
    }
}

#[test]
fn a_root_of_random_names_has_no_stable_names() {
    let root = IpcRoot::new().unwrap();
    let err = root.endpoint_named("acme").unwrap_err();
    assert!(matches!(err, IpcError::NotStable), "{err}");
}
