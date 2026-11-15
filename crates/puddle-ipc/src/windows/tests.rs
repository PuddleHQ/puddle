// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn first_instance_failures_mean_the_name_is_taken() {
    let p = Path::new(r"\\.\pipe\puddle-x");
    for code in [ERROR_ACCESS_DENIED, ERROR_PIPE_BUSY] {
        let err = bind_error(
            p,
            io::Error::from_raw_os_error(i32::try_from(code).unwrap()),
        );
        assert!(matches!(err, IpcError::NameTaken { .. }), "{code}: {err}");
    }
    let err = bind_error(p, io::Error::from_raw_os_error(87));
    assert!(matches!(err, IpcError::Io { op: "create", .. }), "{err}");
}

#[test]
fn endpoint_names_have_the_prefix_and_128_random_bits() {
    let p = Root::new().unwrap().endpoint_path().unwrap();
    let s = p.to_str().unwrap();
    let suffix = s.strip_prefix(PIPE_PREFIX).unwrap();
    assert_eq!(suffix.len(), 32, "{s}");
}

#[tokio::test]
async fn a_pipe_served_by_another_user_is_refused_and_ours_is_not() {
    let root = crate::IpcRoot::new().unwrap();
    let listener = root.listen().unwrap();
    let path = listener.endpoint().path().to_path_buf();
    connect_as(&path, puddle_fs::win::current_user_sid)
        .await
        .unwrap();
    // The local SYSTEM account stands for the other user.
    let err = connect_as(&path, || Ok("S-1-5-18".to_owned()))
        .await
        .unwrap_err();
    assert!(matches!(err, IpcError::WrongOwner { .. }), "{err}");
    crate::connect(&path).await.unwrap();
}

#[test]
fn busy_is_recognised_by_its_os_code() {
    let busy = i32::try_from(ERROR_PIPE_BUSY).unwrap();
    assert!(is_busy(&io::Error::from_raw_os_error(busy)));
    assert!(!is_busy(&io::Error::from_raw_os_error(2)));
    assert!(!is_busy(&io::Error::other("x")));
}

#[test]
fn stable_names_carry_the_install_id_and_the_workspace() {
    let id = "0123456789abcdef0123456789abcdef";
    let root = Root::stable(id).unwrap();
    let p = root.named_path("acme").unwrap();
    assert_eq!(
        p.to_str().unwrap(),
        format!(r"\\.\pipe\puddle-{id}-acme").as_str()
    );
    let err = Root::new().unwrap().named_path("acme").unwrap_err();
    assert!(matches!(err, IpcError::NotStable), "{err}");
}

#[tokio::test]
async fn a_stable_pipe_is_the_same_after_the_process_that_served_it_is_gone() {
    let state = tempfile::tempdir().unwrap();
    let first = crate::IpcRoot::stable(state.path()).unwrap();
    let listener = first.listen_named("acme").unwrap();
    let path = listener.endpoint().path().to_path_buf();
    // A second server of the same name is refused while the first lives.
    let err = first.listen_named("acme").unwrap_err();
    assert!(matches!(err, IpcError::NameTaken { .. }), "{err}");
    listener.close().await;
    let second = crate::IpcRoot::stable(state.path()).unwrap();
    assert_eq!(second.endpoint_named("acme").unwrap().path(), path);
    let _again = second.listen_named("acme").unwrap();
}
