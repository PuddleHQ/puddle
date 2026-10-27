// SPDX-License-Identifier: GPL-3.0-or-later
//! The `editor` stream through a real agent and the host's session code over a Unix socket: the
//! host reaches the editor server's port (and only that port) and reads its password. A plain echo
//! server on the port stands in for the editor server.
#![cfg(unix)]
#![expect(
    clippy::panic,
    clippy::unwrap_used,
    unreachable_pub,
    reason = "helpers outside #[test] functions fail the test by panicking"
)]

#[expect(dead_code, reason = "shared harness; this test uses part of it")]
mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use puddle_agent::config::Target;
use puddle_agent::editor::EditorConfig;
use puddle_agent::{Agent, Config};
use puddle_agent_proto::editor::EditorServer;
use puddle_agent_proto::forward::{GuestLink, OpenError};
use puddle_agent_proto::host::{GuestStream, HostConfig, StreamHandler, serve_session};
use puddle_front::Editor;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, UnixListener};

#[derive(Clone, Default)]
struct Host(Arc<Mutex<Option<GuestLink>>>);

impl StreamHandler for Host {
    async fn handle(&self, _stream: GuestStream) {}

    async fn guest_link(&self, link: GuestLink) {
        *self.0.lock().unwrap() = Some(link);
    }
}

struct Rig {
    host: Host,
    _agent: Agent,
    _dir: common::TempDir,
}

impl Rig {
    async fn link(&self) -> GuestLink {
        for _ in 0..600 {
            if let Some(link) = self.host.0.lock().unwrap().clone() {
                return link;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the agent did not say hello");
    }
}

/// An echo server on the loopback, standing in for the editor server.
async fn echo() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut conn, _)) = listener.accept().await {
            tokio::spawn(async move {
                let (mut r, mut w) = conn.split();
                let _ = tokio::io::copy(&mut r, &mut w).await;
            });
        }
    });
    port
}

/// An agent whose editor server is on `port` with `password` already in its file (the server
/// itself is not started: its port is taken, as by a server an earlier agent started), or no
/// editor at all.
async fn rig(tag: &str, editor: Option<(u16, Option<&str>)>) -> Rig {
    let dir = common::TempDir::new(tag);
    let socket = dir.0.join("route.sock");
    let host = Host::default();
    let listener = UnixListener::bind(&socket).unwrap();
    let handler = host.clone();
    tokio::spawn(async move {
        loop {
            let (conn, _) = listener.accept().await.unwrap();
            let handler = handler.clone();
            tokio::spawn(async move {
                let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
                let _ = serve_session(
                    conn,
                    common::workspace(),
                    Arc::new(common::ChanSink(tx)),
                    Arc::new(handler),
                    HostConfig::default(),
                )
                .await;
            });
        }
    });
    let editor = editor.map(|(port, password)| {
        let password_file = dir.0.join("run/editor-password");
        if let Some(password) = password {
            std::fs::create_dir_all(password_file.parent().unwrap()).unwrap();
            std::fs::write(&password_file, password).unwrap();
        }
        let mut server = EditorServer::new("/nonexistent/data", "/nonexistent/work", 120);
        server.dir = "/nonexistent/release".into();
        server.port = port;
        EditorConfig {
            server,
            password_file,
        }
    });
    let config = Config {
        listen: "127.0.0.1:0".parse().unwrap(),
        target: Target::Unix(socket),
        oom: None,
        bridge: None,
        editor,
        ..Config::default()
    };
    Rig {
        host,
        _agent: Agent::start(config).await.unwrap(),
        _dir: dir,
    }
}

const PASSWORD: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

#[tokio::test]
async fn the_host_reaches_the_editor_port_and_reads_its_password() {
    let port = echo().await;
    let rig = rig("editor-stream", Some((port, Some(PASSWORD)))).await;
    let link = rig.link().await;
    let mut stream = link.open_editor().await.unwrap();
    stream.write_all(b"GET /healthz").await.unwrap();
    let mut back = [0_u8; 12];
    stream.read_exact(&mut back).await.unwrap();
    assert_eq!(&back, b"GET /healthz");
    assert_eq!(link.editor_password().await.unwrap(), PASSWORD);
}

#[tokio::test]
async fn a_password_file_with_a_trailing_newline_is_sent_without_it() {
    let port = echo().await;
    let rig = rig(
        "editor-newline",
        Some((port, Some(&format!("{PASSWORD}\n")))),
    )
    .await;
    assert_eq!(rig.link().await.editor_password().await.unwrap(), PASSWORD);
}

#[tokio::test]
async fn before_the_server_has_a_password_the_request_is_refused_with_a_reason() {
    let port = echo().await;
    let rig = rig("editor-nopassword", Some((port, None))).await;
    let err = rig.link().await.editor_password().await.unwrap_err();
    assert!(
        matches!(&err, OpenError::Refused(why) if why.contains("not started")),
        "{err}"
    );
}

#[tokio::test]
async fn nothing_listening_on_the_editor_port_is_refused_with_a_reason() {
    let free = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let rig = rig("editor-down", Some((free, Some(PASSWORD)))).await;
    let err = rig.link().await.open_editor().await.unwrap_err();
    assert!(
        matches!(&err, OpenError::Refused(why) if why.contains("listens")),
        "{err}"
    );
}

#[tokio::test]
async fn a_sandbox_without_an_editor_server_refuses_both_requests() {
    let rig = rig("editor-none", None).await;
    let link = rig.link().await;
    for err in [
        link.open_editor().await.unwrap_err(),
        link.editor_password().await.unwrap_err(),
    ] {
        assert!(
            matches!(&err, OpenError::Refused(why) if why.contains("no editor server")),
            "{err}"
        );
    }
}

#[tokio::test]
async fn a_forward_still_refuses_the_editor_port() {
    let port = echo().await;
    let rig = rig("editor-forward", Some((port, Some(PASSWORD)))).await;
    let err = rig.link().await.open_forward(port).await.unwrap_err();
    assert!(
        matches!(&err, OpenError::Refused(why) if why.contains("belongs to Puddle")),
        "{err}"
    );
}

#[tokio::test]
async fn the_front_reaches_the_editor_through_the_link_and_hears_why_when_it_cannot() {
    let port = echo().await;
    let rig = rig("editor-front", Some((port, Some(PASSWORD)))).await;
    let link = rig.link().await;
    let mut stream = Editor::connect(&link).await.unwrap();
    stream.write_all(b"ping").await.unwrap();
    let mut back = [0_u8; 4];
    stream.read_exact(&mut back).await.unwrap();
    assert_eq!(&back, b"ping");
    assert_eq!(Editor::password(&link).await.unwrap(), PASSWORD);

    let none = self::rig("editor-front-none", None).await;
    let link = none.link().await;
    let connect = Editor::connect(&link).await.err().unwrap();
    assert!(connect.0.contains("no editor server"), "{connect}");
    let password = Editor::password(&link).await.unwrap_err();
    assert!(password.0.contains("no editor server"), "{password}");
}
