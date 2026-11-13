// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;

/// Answers one connection with `reply`, then closes; returns where it listens.
fn answering(reply: &'static [u8]) -> SocketAddr {
    use std::io::{Read, Write};
    let (listener, port) = bound_port();
    std::thread::spawn(move || {
        if let Ok((mut conn, _)) = listener.accept() {
            let mut buf = [0u8; 512];
            let _ = conn.read(&mut buf);
            let _ = conn.write_all(reply);
        }
    });
    SocketAddr::from(([127, 0, 0, 1], port))
}

#[test]
fn an_http_status_is_read_from_what_answers_and_nothing_from_what_does_not() {
    let ok = answering(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n");
    assert_eq!(http_status(ok, "/x", "t"), Some(200));
    let refused = answering(b"HTTP/1.1 401 Unauthorized\r\n\r\n");
    assert_eq!(http_status(refused, "/x", "t"), Some(401));
    let stranger = answering(b"SSH-2.0-not-http\r\n");
    assert_eq!(http_status(stranger, "/x", "t"), None);
    let silent = answering(b"");
    assert_eq!(http_status(silent, "/x", "t"), None);
    // Reserved, not freed: nothing can answer there, and no parallel test can take the number.
    let reserved = ReservedPort::new();
    let gone = SocketAddr::from(([127, 0, 0, 1], reserved.port()));
    assert_eq!(http_status(gone, "/x", "t"), None);
}

#[test]
fn two_bound_ports_differ_and_stay_taken() {
    let (first, a) = bound_port();
    let (second, b) = bound_port();
    assert_ne!(a, b);
    assert!(TcpListener::bind(("127.0.0.1", a)).is_err());
    drop((first, second));
}

#[test]
fn a_reserved_port_refuses_connections_and_other_binds_but_takes_a_reusing_listener() {
    let reserved = ReservedPort::new();
    let addr = SocketAddr::from(([127, 0, 0, 1], reserved.port()));
    assert!(!accepts(addr), "nothing listens on a reserved port");
    // Another program picking the number without SO_REUSEADDR is turned away: the collision a
    // dropped listener leaves open.
    let squatter = tokio::net::TcpSocket::new_v4().unwrap();
    assert!(squatter.bind(addr).is_err(), "a plain bind took the port");
    let rt = runtime();
    let server = listen_reusing(&rt, addr);
    assert!(accepts(addr), "the test's own server answers");
    drop(server);
    refused_soon(addr); // and be dropped again
    let other = ReservedPort::default();
    assert_ne!(other.port(), reserved.port());
}

#[test]
fn a_quiet_port_is_below_the_ephemeral_ranges_and_can_be_bound_plainly() {
    let port = quiet_port();
    assert!((20_000..32_000).contains(&port), "{port}");
    // The probe that found the port closed its socket, but a forking test thread can hold a copy
    // of it until the child execs (see `refused_soon`), so a bind straight after can see it taken.
    let bound = crate::wait::eventually_blocking(
        "a plain bind takes the quiet port",
        Duration::from_secs(10),
        || TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], port))),
    );
    drop(bound);
}

#[test]
fn two_picks_that_start_at_the_same_point_get_different_ports() {
    // Two processes that pick the same number in the same moment: before the claim, both were
    // handed it (the probe frees it again) and one lost the bind later.
    let range = (30_000, 200);
    let first = free_port_from(range.0, range.1, 17, 0).unwrap();
    let second = free_port_from(range.0, range.1, 17, 0).unwrap();
    assert_ne!(first, second);
}

#[test]
fn a_range_with_every_port_held_has_none_free() {
    let (_held, port) = bound_port();
    assert_eq!(free_port_in(port, 1), None);
}

/// Waits until nothing answers on `addr`. A listener that was just dropped can still answer for a
/// moment: another test thread that forks (`Command::spawn`) copies every open descriptor, the
/// listening socket among them, and the child keeps its copy until it execs. The socket is closed
/// for good a moment later, so "refused after the drop" holds eventually, not at once.
fn refused_soon(addr: SocketAddr) {
    crate::wait::eventually_blocking(
        "nothing answers on a dropped listener's port",
        Duration::from_secs(10),
        || !accepts(addr),
    );
}

/// A current-thread runtime for the tests that need a tokio socket.
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

/// A listener on a [`ReservedPort`]'s number, bound with `SO_REUSEADDR` like a real server
/// (serve.rs): std's plain bind would be refused on Windows, where only two sockets that both set
/// it can share. Drop it and the port is refused again, still the reservation's.
fn listen_reusing(rt: &tokio::runtime::Runtime, addr: SocketAddr) -> tokio::net::TcpListener {
    rt.block_on(async {
        let socket = tokio::net::TcpSocket::new_v4().unwrap();
        socket.set_reuseaddr(true).unwrap();
        socket
            .bind(addr)
            .expect("the test's own server can answer there");
        socket.listen(8).unwrap()
    })
}

#[test]
fn a_listener_is_accepted_and_a_closed_port_refused() {
    // The port stays reserved while the listener comes and goes: a freed port (`bound_port` then
    // drop) is open to any parallel test's port 0 bind between the drop and the check.
    let reserved = ReservedPort::new();
    let addr = SocketAddr::from(([127, 0, 0, 1], reserved.port()));
    let rt = runtime();
    let listener = listen_reusing(&rt, addr);
    assert!(accepts(addr));
    drop(listener);
    refused_soon(addr);
}

#[test]
fn a_socket_connected_to_itself_is_not_accepted() {
    // The kernel's simultaneous open: bind to a port, then connect to that same port with
    // nothing listening. The connect succeeds, to nobody; `is_ok()` on it would read "open".
    let reserved = ReservedPort::new();
    let addr = SocketAddr::from(([127, 0, 0, 1], reserved.port()));
    let rt = runtime();
    let joined = rt.block_on(async {
        let socket = tokio::net::TcpSocket::new_v4().unwrap();
        socket.set_reuseaddr(true).unwrap();
        socket.bind(addr).unwrap();
        socket.connect(addr).await
    });
    let stream = joined.expect("a socket can connect to its own address and port");
    let stream = stream.into_std().unwrap();
    assert_eq!(stream.local_addr().unwrap(), stream.peer_addr().unwrap());
    assert!(
        !has_a_peer(&stream),
        "a socket joined to itself has no peer"
    );
    // While that socket holds the address, a connect from outside finds nobody either.
    assert!(!accepts(addr));
}

#[cfg(unix)]
#[test]
fn a_private_tempdir_is_owner_only_whatever_the_umask() {
    use std::os::unix::fs::PermissionsExt;
    let dir = private_tempdir();
    let mode = std::fs::metadata(dir.path()).unwrap().permissions().mode();
    assert_eq!(mode & 0o7777, 0o700, "{}", dir.path().display());
}

#[test]
fn a_test_dir_is_unique_labelled_and_removed_on_drop() {
    let one = TestDir::new("kit-a-");
    let two = TestDir::new("kit-a-");
    assert_ne!(one.as_ref() as &Path, two.as_ref() as &Path);
    assert!(
        one.file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("kit-a-")
    );
    std::fs::write(one.join("f"), "x").unwrap();
    let path = one.to_path_buf();
    drop(one);
    assert!(!path.exists());
}
