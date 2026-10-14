// SPDX-License-Identifier: GPL-3.0-or-later
//! Hostile-guest tier W: a real second local user (a throwaway account the test makes and logs
//! on) goes after a route's pipe. The guest can only reach a route through msb, which runs as the
//! owner, so the second user stands for anything else on the machine that wants to be the route's
//! peer: connect to it, add its own instance to it, or create the name first.
//!
//! Where the runner will not make accounts the tests skip and say so, unless
//! `PUDDLE_REQUIRE_SECOND_USER=1` (the Windows workflows set it), which turns that into a failure.
#![cfg(windows)]
#![expect(
    clippy::unwrap_used,
    reason = "helpers run outside #[test] but only in tests"
)]

use std::io;
use std::path::Path;

use puddle_fs::win::testing::{SecondUser, assert_access_denied};
use puddle_ipc::{IpcError, IpcRoot, Listener, connect};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use windows_sys::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE};

/// Echoes what the first client sends, once, so the owner can show the route still works.
fn spawn_echo(mut listener: Listener) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let Ok(mut conn) = listener.accept().await else {
            return;
        };
        let mut buf = [0u8; 5];
        if conn.read_exact(&mut buf).await.is_ok() {
            let _ = conn.write_all(&buf).await;
            let _ = conn.flush().await;
            // Keep the instance open until the client has read the echo.
            let _ = conn.read(&mut [0u8; 1]).await;
        }
    })
}

async fn owner_roundtrip(path: &Path) {
    let mut client = connect(path).await.unwrap();
    client.write_all(b"owner").await.unwrap();
    let mut back = [0u8; 5];
    client.read_exact(&mut back).await.unwrap();
    assert_eq!(&back, b"owner");
}

/// A pipe with Windows' default descriptor (read for everyone), for the controls.
fn control_pipe() -> (String, NamedPipeServer) {
    let mut random = [0u8; 8];
    getrandom::fill(&mut random).unwrap();
    let path = format!(
        r"\\.\pipe\puddle-hg24-control-{:016x}",
        u64::from_le_bytes(random)
    );
    let server = ServerOptions::new()
        .first_pipe_instance(true)
        .create(&path)
        .unwrap();
    (path, server)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hostile_hg24_a_second_user_cannot_connect_to_a_live_route() {
    let Some(user) = SecondUser::for_test() else {
        return;
    };
    // Control first: the same account can open a pipe that has Windows' default descriptor, so
    // the refusals below come from the route's owner-only descriptor.
    let (control, _server) = control_pipe();
    user.open(Path::new(&control), GENERIC_READ).unwrap();

    let root = IpcRoot::new().unwrap();
    let listener = root.listen().unwrap();
    let path = listener.endpoint().path().to_path_buf();
    let echo = spawn_echo(listener);
    for access in [GENERIC_READ, GENERIC_WRITE, GENERIC_READ | GENERIC_WRITE] {
        assert_access_denied(user.open(&path, access));
    }
    // The refusals changed nothing for the owner.
    owner_roundtrip(&path).await;
    echo.await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hostile_hg24_a_second_user_cannot_add_an_instance_to_a_live_route() {
    let Some(user) = SecondUser::for_test() else {
        return;
    };
    let runtime = tokio::runtime::Handle::current();
    let root = IpcRoot::new().unwrap();
    let listener = root.listen().unwrap();
    let path = listener.endpoint().path().to_path_buf();
    let echo = spawn_echo(listener);

    let (control, _server) = control_pipe();
    let create = |name: String, first: bool| {
        let runtime = runtime.clone();
        user.run(move || {
            let _enter = runtime.enter();
            ServerOptions::new()
                .first_pipe_instance(first)
                .create(&name)
                .map(drop)
        })
    };
    // Control: the account can create pipes in general (a name of its own), so the two refusals
    // below come from the route's descriptor.
    create(format!("{control}-own"), true).unwrap();
    // The attack: a second server instance on puddle's route name, to catch clients that connect
    // to the route (a squatter among the instances).
    assert_access_denied(create(path.display().to_string(), false));
    assert_access_denied(create(path.display().to_string(), true));

    owner_roundtrip(&path).await;
    echo.await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hostile_hg24_a_route_name_a_second_user_created_first_is_refused() {
    let Some(user) = SecondUser::for_test() else {
        return;
    };
    let runtime = tokio::runtime::Handle::current();
    let root = IpcRoot::new().unwrap();
    let endpoint = root.endpoint().unwrap();
    let name = endpoint.path().to_path_buf();

    // The squatter creates puddle's name before puddle binds it (with a descriptor of its own).
    let squatter: io::Result<NamedPipeServer> = user.run({
        let name = name.clone();
        move || {
            let _enter = runtime.enter();
            ServerOptions::new().first_pipe_instance(true).create(&name)
        }
    });
    let squatter = squatter.unwrap();

    // Puddle refuses, binds nothing, and so a route over this name is never started.
    let err = root.bind(&endpoint).unwrap_err();
    assert!(matches!(err, IpcError::NameTaken { .. }), "{err}");
    assert!(err.to_string().contains(&endpoint.to_string()), "{err}");

    // With the squatter gone the same name binds, and the owner's client reaches puddle's listener.
    drop(squatter);
    let listener = root.bind(&endpoint).unwrap();
    let echo = spawn_echo(listener);
    owner_roundtrip(&name).await;
    echo.await.unwrap();
}
