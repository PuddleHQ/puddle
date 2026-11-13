// SPDX-License-Identifier: AGPL-3.0-or-later
//! The guest's SSH agent socket through a real agent and the host's session code over a Unix
//! socket: a client of the socket becomes one `ssh-agent` stream on the host, and the bytes go
//! through both ways unchanged.
#![cfg(unix)]
#![expect(
    clippy::unwrap_used,
    reason = "helpers outside #[test] functions fail the test by panicking"
)]

use std::os::unix::fs::PermissionsExt as _;
use std::sync::Arc;
use std::time::Duration;

use puddle_agent::config::{SshAgentConfig, Target};
use puddle_agent::{Agent, Config};
use puddle_agent_proto::host::{
    AgentStream, GuestStream, HostConfig, StreamHandler, serve_session,
};
use puddle_test_kit::eventually;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

use crate::common;

/// A host whose filter answers each agent request with `host:` and the request.
#[derive(Clone)]
struct Host;

impl StreamHandler for Host {
    async fn handle(&self, _stream: GuestStream) {}

    async fn ssh_agent(&self, mut stream: AgentStream) {
        let mut buf = [0u8; 64];
        while let Ok(n) = stream.read(&mut buf).await {
            if n == 0 {
                return;
            }
            let mut answer = b"host:".to_vec();
            answer.extend_from_slice(buf.get(..n).unwrap());
            if stream.write_all(&answer).await.is_err() {
                return;
            }
        }
    }
}

#[tokio::test]
async fn a_client_of_the_guest_socket_reaches_the_hosts_agent_stream_both_ways() {
    let dir = common::TempDir::new("ssh-agent");
    let route = dir.0.join("route.sock");
    let socket = dir.0.join("run/ssh-agent.sock");
    let listener = UnixListener::bind(&route).unwrap();
    tokio::spawn(async move {
        loop {
            let (conn, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
                let _ = serve_session(
                    conn,
                    common::workspace(),
                    Arc::new(common::ChanSink(tx)),
                    Arc::new(Host),
                    HostConfig::default(),
                )
                .await;
            });
        }
    });
    let config = Config {
        listen: "127.0.0.1:0".parse().unwrap(),
        target: Target::Unix(route),
        oom: None,
        ports: None,
        ssh_agent: Some(SshAgentConfig {
            socket: socket.clone(),
            user: "puddle-no-such-user".to_owned(),
            owner_wait: Duration::from_millis(10),
        }),
        ..Config::default()
    };
    let _agent = Agent::start(config).await.unwrap();
    eventually("the agent socket", Duration::from_secs(6), || {
        socket.exists().then_some(())
    })
    .await;
    let mode = std::fs::metadata(&socket).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);

    let mut client = UnixStream::connect(&socket).await.unwrap();
    client.write_all(b"list").await.unwrap();
    let mut answer = [0u8; 9];
    tokio::time::timeout(Duration::from_secs(6), client.read_exact(&mut answer))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(&answer, b"host:list");
}
