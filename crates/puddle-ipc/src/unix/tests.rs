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
