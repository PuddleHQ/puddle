// SPDX-License-Identifier: GPL-3.0-or-later
//! Hostile-guest tier W: a real second local user (a throwaway account the test makes and logs
//! on) against the API and its connection file. The API listens on loopback, where any local user
//! can connect, so the token is the only gate and the connection file is the only place it is
//! kept: the second user must not be able to read it, and without it every route says 401.
//!
//! Where the runner will not make accounts the tests skip and say so, unless
//! `PUDDLE_REQUIRE_SECOND_USER=1` (the Windows workflows set it), which turns that into a failure.

use std::io::{Read, Write};
use std::net::TcpStream;

use puddle_api::{ApiToken, ConnectionFileError, ConnectionInfo};
use puddle_fs::win::testing::{SecondUser, SharedDir, assert_access_denied};
use windows_sys::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE};

use crate::common::start;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hostile_hg25_a_second_user_cannot_read_or_change_the_connection_file() {
    let Some(user) = SecondUser::for_test() else {
        return;
    };
    let api = start().await;
    // The file holds a token of its own: the point is who can read it, not which token it is.
    let info = ConnectionInfo {
        url: format!("http://{}", api.addr),
        token: ApiToken::generate().unwrap(),
    };
    let dir = SharedDir::new("puddle-api");
    let file = dir.path().join("api.json");
    info.write(&file).unwrap();
    // Control: an ordinary file in the same folder is readable by the account, so the refusals
    // come from the connection file's owner-only ACL and not from the folder.
    let plain = dir.path().join("plain.txt");
    std::fs::write(&plain, b"x").unwrap();
    user.open(&plain, GENERIC_READ).unwrap();

    for access in [GENERIC_READ, GENERIC_WRITE, GENERIC_READ | GENERIC_WRITE] {
        assert_access_denied(user.open(&file, access));
    }
    // The product's own reader, run as the second user, refuses too, and its message holds no token.
    let err = user.run(|| ConnectionInfo::read(&file)).unwrap_err();
    assert!(
        matches!(&err, ConnectionFileError::Io { source, .. }
            if source.kind() == std::io::ErrorKind::PermissionDenied),
        "{err}"
    );
    assert!(
        !format!("{err} {err:?}").contains(info.token.expose()),
        "{err}"
    );

    // The owner still reads it back, token and URL intact.
    let read = ConnectionInfo::read(&file).unwrap();
    assert_eq!(read.token.expose(), info.token.expose());
    assert_eq!(read.url, info.url);
    api.running.shutdown().await;
}

/// One request as the second user, without any token; the status line and the whole answer.
fn get_as_second_user(
    user: &SecondUser,
    addr: std::net::SocketAddr,
    path: &str,
) -> (String, String) {
    user.run(|| {
        let mut conn = TcpStream::connect(addr).unwrap();
        write!(
            conn,
            "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\n\r\n",
            addr.port()
        )
        .unwrap();
        let mut answer = String::new();
        conn.read_to_string(&mut answer).unwrap();
        let status = answer.lines().next().unwrap_or_default().to_owned();
        (status, answer)
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hostile_hg25_a_second_user_without_the_token_is_refused_on_every_door() {
    let Some(user) = SecondUser::for_test() else {
        return;
    };
    let api = start().await;
    let addr = api.addr;
    // The blocking client runs on its own thread; the API keeps serving on the runtime's.
    for path in [
        "/api/health",
        "/api/pending",
        "/api/rules",
        "/api/workspaces",
        "/api/openapi.json",
    ] {
        let (status, answer) =
            tokio::task::block_in_place(|| get_as_second_user(&user, addr, path));
        assert!(status.contains(" 401 "), "{path}: {status}");
        assert!(!answer.contains(&api.token), "{path}: the token leaked");
        assert!(
            !answer.to_ascii_lowercase().contains("access-control-allow"),
            "{path}: no CORS headers, ever"
        );
    }
    api.running.shutdown().await;
}
