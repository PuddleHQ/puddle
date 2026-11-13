// SPDX-License-Identifier: AGPL-3.0-or-later
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

/// This process's user name, from `/etc/passwd` and the owner of a file it just made.
fn this_user(dir: &Path) -> String {
    use std::os::unix::fs::MetadataExt as _;
    let uid = std::fs::metadata(dir).unwrap().uid().to_string();
    std::fs::read_to_string("/etc/passwd")
        .unwrap()
        .lines()
        .find_map(|l| {
            let mut f = l.split(':');
            let name = f.next()?;
            (f.nth(1)? == uid).then(|| name.to_owned())
        })
        .expect("this process's user is in /etc/passwd")
}

#[tokio::test]
async fn the_socket_goes_to_the_session_user_and_a_vanished_socket_stays_unowned() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("ssh-agent.sock");
    let _listener = bind(&socket).unwrap();
    let mut config = SshAgentConfig {
        socket: socket.clone(),
        user: this_user(dir.path()),
        owner_wait: Duration::from_millis(10),
    };
    assert!(own(&config).await);
    config.socket = dir.path().join("gone.sock");
    assert!(!own(&config).await);
}

fn unreachable_host() -> Arc<Upstream> {
    Arc::new(Upstream::new(&crate::Config {
        target: crate::config::Target::Unix("/nonexistent/puddle-route.sock".into()),
        ..crate::Config::default()
    }))
}

#[tokio::test]
async fn a_client_is_closed_when_the_host_cannot_be_reached() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("ssh-agent.sock");
    let config = SshAgentConfig {
        socket: socket.clone(),
        user: "no-such-user-of-this-test".to_owned(),
        owner_wait: Duration::from_millis(10),
    };
    let task = tokio::spawn(serve(config, unreachable_host()));
    puddle_test_kit::eventually("the agent socket", Duration::from_secs(5), || {
        socket.exists().then_some(())
    })
    .await;
    let mut client = UnixStream::connect(&socket).await.unwrap();
    let mut rest = Vec::new();
    let read = tokio::time::timeout(
        Duration::from_secs(5),
        tokio::io::AsyncReadExt::read_to_end(&mut client, &mut rest),
    )
    .await
    .expect("the client is closed");
    assert!(read.is_err() || rest.is_empty(), "{read:?} {rest:?}");
    task.abort();
}

#[tokio::test]
async fn a_socket_that_cannot_be_made_ends_the_service_without_one() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("a-file");
    std::fs::write(&file, b"").unwrap();
    let config = SshAgentConfig {
        socket: file.join("ssh-agent.sock"),
        user: "no-such-user-of-this-test".to_owned(),
        owner_wait: Duration::from_millis(10),
    };
    tokio::time::timeout(Duration::from_secs(5), serve(config, unreachable_host()))
        .await
        .expect("serve returns at once");
}
