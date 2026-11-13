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
