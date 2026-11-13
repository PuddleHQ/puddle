// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[tokio::test]
async fn a_server_run_by_another_user_is_refused_and_ours_is_not() {
    let root = crate::IpcRoot::new().unwrap();
    let listener = root.listen().unwrap();
    let path = listener.endpoint().path().to_path_buf();
    let own = rustix::process::geteuid().as_raw();
    connect_as(&path, own).await.unwrap();
    let err = connect_as(&path, own.wrapping_add(1)).await.unwrap_err();
    assert!(matches!(err, IpcError::WrongOwner { .. }), "{err}");
    // The public entry point asks for the real user.
    crate::connect(&path).await.unwrap();
}

#[test]
fn long_paths_are_refused_before_bind() {
    let long = PathBuf::from(format!("/{}", "a".repeat(MAX_SOCKET_PATH)));
    let err = check_length(&long).unwrap_err();
    assert!(matches!(err, IpcError::PathTooLong { len, .. } if len == MAX_SOCKET_PATH + 1));
    let ok = PathBuf::from(format!("/{}", "a".repeat(MAX_SOCKET_PATH - 1)));
    check_length(&ok).unwrap();
}

#[test]
fn default_parent_is_an_existing_absolute_dir() {
    let p = default_parent();
    assert!(p.is_absolute() && p.is_dir(), "{}", p.display());
}

#[test]
fn a_root_in_a_missing_parent_fails_with_the_path() {
    let tmp = tempfile::tempdir().unwrap();
    let parent = tmp.path().join("missing");
    let err = Root::new_in(&parent).unwrap_err();
    match err {
        IpcError::Root { path, source } => {
            assert!(path.starts_with(&parent));
            assert_eq!(source.kind(), io::ErrorKind::NotFound);
        }
        other => panic!("unexpected {other}"),
    }
}

fn stable_root(parent: &Path, state: &Path) -> crate::IpcRoot {
    crate::IpcRoot::stable_in(parent, state).unwrap()
}

#[tokio::test]
async fn a_stable_endpoint_is_the_same_after_the_process_that_served_it_is_gone() {
    let parent = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let first = stable_root(parent.path(), state.path());
    let listener = first.listen_named("acme").unwrap();
    let path = listener.endpoint().path().to_path_buf();
    crate::connect(&path).await.unwrap();
    drop(listener);
    drop(first);
    // A new run: a new root over the same state folder names the same socket.
    let second = stable_root(parent.path(), state.path());
    assert_eq!(second.endpoint_named("acme").unwrap().path(), path);
    let mut listener = second.listen_named("acme").unwrap();
    let client = tokio::spawn(async move { crate::connect(&path).await.map(drop) });
    listener.accept().await.unwrap();
    client.await.unwrap().unwrap();
    // Another install gets other names.
    let other_state = tempfile::tempdir().unwrap();
    let other = stable_root(parent.path(), other_state.path());
    assert_ne!(
        other.endpoint_named("acme").unwrap().path(),
        second.endpoint_named("acme").unwrap().path()
    );
}

#[tokio::test]
async fn a_socket_file_a_dead_process_left_is_replaced() {
    let parent = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let root = stable_root(parent.path(), state.path());
    let path = root.endpoint_named("acme").unwrap().path().to_path_buf();
    // A killed Puddle leaves the file: bound and closed with nobody removing it.
    drop(std::os::unix::net::UnixListener::bind(&path).unwrap());
    assert!(path.exists());
    let mut listener = root.listen_named("acme").unwrap();
    let client = tokio::spawn({
        let path = path.clone();
        async move { crate::connect(&path).await.map(drop) }
    });
    listener.accept().await.unwrap();
    client.await.unwrap().unwrap();
}

#[tokio::test]
async fn a_socket_that_still_answers_is_never_taken_over() {
    let parent = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let root = stable_root(parent.path(), state.path());
    let _live = root.listen_named("acme").unwrap();
    // Another Puddle of the same user, or a squatter: the same name is refused, the live one stays.
    let again = stable_root(parent.path(), state.path());
    let err = again.listen_named("acme").unwrap_err();
    assert!(matches!(err, IpcError::NameTaken { .. }), "{err}");
    crate::connect(root.endpoint_named("acme").unwrap().path())
        .await
        .unwrap();
}

#[test]
fn a_root_folder_that_is_not_ours_alone_is_refused() {
    let parent = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let id = crate::install_id(state.path()).unwrap();
    let folder = parent.path().join(format!("puddle-{}", &id[..8]));
    // Too open: others could add sockets to it.
    fs::create_dir(&folder).unwrap();
    fs::set_permissions(&folder, Permissions::from_mode(0o755)).unwrap();
    let err = crate::IpcRoot::stable_in(parent.path(), state.path()).unwrap_err();
    assert!(matches!(err, IpcError::Root { .. }), "{err}");
    fs::remove_dir(&folder).unwrap();
    // A symlink is never followed, even to a folder with the right mode.
    let target = parent.path().join("elsewhere");
    puddle_fs::private::create_new_dir(&target).unwrap();
    std::os::unix::fs::symlink(&target, &folder).unwrap();
    let err = crate::IpcRoot::stable_in(parent.path(), state.path()).unwrap_err();
    assert!(matches!(err, IpcError::Root { .. }), "{err}");
    fs::remove_file(&folder).unwrap();
    // A missing one is made 0700, and an existing 0700 one is reused.
    crate::IpcRoot::stable_in(parent.path(), state.path()).unwrap();
}
