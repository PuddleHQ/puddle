// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

fn upstream(target: Target) -> Upstream {
    Upstream::new(&Config {
        target,
        mux: 2,
        ..Config::default()
    })
}

#[tokio::test]
async fn an_unreachable_host_is_an_error_on_every_slot() {
    let up = upstream(Target::Unix("/nonexistent/puddle-route.sock".into()));
    assert!(format!("{up:?}").contains("sessions: 2"));
    for _ in 0..3 {
        assert!(up.open().await.is_err());
    }
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_vsock_target_nobody_listens_on_is_an_error() {
    // CID 1 is local loopback; nothing listens there.
    let up = upstream(Target::Vsock {
        cid: 1,
        port: 0x7fff_fff1,
    });
    assert!(up.open().await.is_err());
}

#[cfg(windows)]
#[tokio::test]
async fn a_named_pipe_is_dialled_and_a_missing_one_is_an_error() {
    use tokio::net::windows::named_pipe::ServerOptions;

    let name = format!(r"\\.\pipe\puddle-agent-dial-{}", std::process::id());
    let _server = ServerOptions::new().create(&name).unwrap();
    dial_unix(std::path::Path::new(&name)).await.unwrap();
    let missing = format!(r"{name}-missing");
    assert!(dial_unix(std::path::Path::new(&missing)).await.is_err());
}
