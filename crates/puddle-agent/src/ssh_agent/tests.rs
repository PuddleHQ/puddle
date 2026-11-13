// SPDX-License-Identifier: GPL-3.0-or-later
use std::os::unix::fs::PermissionsExt as _;

use super::*;

#[tokio::test]
async fn the_socket_is_made_in_its_folder_mode_0600_and_replaces_an_old_one() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("run/puddle/ssh-agent.sock");
    let first = bind(&socket).unwrap();
    drop(first);
    // The old agent's socket file is still there; a new agent binds over it.
    let listener = bind(&socket).unwrap();
    let mode = std::fs::metadata(&socket).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    let client = tokio::spawn({
        let socket = socket.clone();
        async move { UnixStream::connect(socket).await.is_ok() }
    });
    listener.accept().await.unwrap();
    assert!(client.await.unwrap());
}

#[tokio::test]
async fn a_session_user_that_never_appears_leaves_the_socket_as_it_is() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("ssh-agent.sock");
    let _listener = bind(&socket).unwrap();
    let config = SshAgentConfig {
        socket,
        user: "no-such-user-of-this-test".to_owned(),
        owner_wait: Duration::from_millis(10),
    };
    assert!(!own(&config).await);
}
