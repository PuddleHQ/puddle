// SPDX-License-Identifier: GPL-3.0-or-later
//! Ports and folders that two parallel tests cannot collide on.

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::ops::Deref;
use std::path::Path;
use std::time::Duration;

/// A loopback listener bound to port 0, with the port the system gave it. Hand the listener to the
/// code under test (or keep it while a child binds its own); never drop it to "free" the port, because
/// another test can take the port before the caller binds it.
///
/// # Panics
/// When the system has no loopback port to give.
#[must_use]
pub fn bound_port() -> (TcpListener, u16) {
    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .expect("bind a loopback port for a test");
    let port = listener
        .local_addr()
        .expect("read the port of a bound test listener")
        .port();
    (listener, port)
}

/// A loopback port that stays this test's until the value is dropped, for a config that must name
/// its port before anything listens on it (a server's `--port`). The socket is bound with
/// `SO_REUSEADDR` and never listens, so connecting to the port is refused, as on a free one.
///
/// What it guarantees depends on the system:
/// - Linux and macOS: no other bind takes the number unless that bind sets `SO_REUSEADDR` too, and
///   port 0 and the kernel's choice of a source port skip it. The test's own listener can bind the
///   same number if it sets `SO_REUSEADDR` (tokio's `TcpListener::bind` does on these systems) and
///   be dropped again as often as the test needs.
/// - Windows: a bind that sets `SO_REUSEADDR` can share the port, and a bind that does not is
///   refused, so a listener made by tokio's `TcpListener::bind` (it sets none there) cannot answer
///   on it at all. Use [`quiet_port`] when the code under test binds the number itself.
///
/// Never drop a listener to "free" a port for this: use this instead.
#[derive(Debug)]
pub struct ReservedPort {
    _socket: tokio::net::TcpSocket,
    port: u16,
}

impl ReservedPort {
    /// Reserves a loopback port.
    ///
    /// # Panics
    /// When the system has no loopback port to give.
    #[must_use]
    pub fn new() -> Self {
        let socket = tokio::net::TcpSocket::new_v4().expect("make a socket to reserve a port");
        socket
            .set_reuseaddr(true)
            .expect("let a test listener share a reserved port");
        socket
            .bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .expect("bind a loopback port to reserve");
        let port = socket.local_addr().expect("read the reserved port").port();
        Self {
            _socket: socket,
            port,
        }
    }

    /// The reserved port number.
    #[must_use]
    pub fn port(&self) -> u16 {
        self.port
    }
}

impl Default for ReservedPort {
    fn default() -> Self {
        Self::new()
    }
}

/// A free loopback port below both systems' ephemeral ranges (Linux from 32768, Windows from
/// 49152), for a test that must name a port a restarted program binds itself, with a plain bind
/// that cannot share (tokio sets no `SO_REUSEADDR` on Windows, so a [`ReservedPort`] there makes
/// that bind fail). Nothing's port 0, source port or the kernel's choice can land on it, which is
/// the collision a port released moments ago is open to. Another test can only reach it by picking
/// the same number from this range at the same moment (the pick starts at a different point in each
/// process and call).
///
/// The port is free when it is returned and not held: a program that is not a test could still bind
/// it.
///
/// Windows keeps ranges out of use for Hyper-V and WSL (`netsh int ipv4 show excludedportrange
/// protocol=tcp`), and a bind to a port in one fails. Most sit in the dynamic range (from 49152),
/// which this range is below; a port that is excluded anyway fails the bind here and is skipped.
///
/// # Panics
/// When no port in the range can be bound.
#[must_use]
pub fn quiet_port() -> u16 {
    free_port_in(20_000, 12_000).expect("a free loopback port between 20000 and 32000")
}

/// The first free loopback port `first + k` for a `k` below `span`, tried from a start that differs
/// per process and call.
fn free_port_in(first: u16, span: u16) -> Option<u16> {
    use std::sync::atomic::{AtomicU16, Ordering};
    static CALLS: AtomicU16 = AtomicU16::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    let start = (nanos ^ std::process::id().wrapping_mul(2_654_435_761)) % u32::from(span);
    let call = u32::from(CALLS.fetch_add(1, Ordering::Relaxed));
    (0..u32::from(span)).find_map(|step| {
        // 7919 is prime; for a span it does not divide, the steps visit every port once.
        let offset = (start + (call + step) * 7_919) % u32::from(span);
        let port = first + u16::try_from(offset).expect("the offset is below the span");
        TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], port)))
            .is_ok()
            .then_some(port)
    })
}

/// Whether a listener accepts a connection on `addr` (loopback only: the connect blocks, up to a
/// second).
///
/// `TcpStream::connect(..).is_ok()` is not that. A connect to a port nothing listens on can still
/// report success: the kernel connects a socket to itself when its source port happens to be the
/// target (a simultaneous open), and tokio's connect can return `Ok` for a refused attempt that is
/// still in flight. Both end up with no peer to talk to, so this asks the connection for its peer
/// and counts a socket joined to itself as refused.
#[must_use]
pub fn accepts(addr: SocketAddr) -> bool {
    let Ok(conn) = TcpStream::connect_timeout(&addr, Duration::from_secs(1)) else {
        return false;
    };
    has_a_peer(&conn)
}

/// The status an HTTP/1.1 server on `addr` gives `GET path` with `Authorization: Bearer token`:
/// `None` when nothing accepts, or what accepts does not speak HTTP (loopback only; each wait is
/// bounded at a second).
///
/// For a test that a server has stopped. `TcpStream::connect(..).is_err()` is not that: a server
/// that bound port 0 gave its number back, and another test's socket can take it before the test
/// connects (measured on a Windows runner: about 1 in 1,000 released ports is taken again within the
/// next connect under 8 to 16 busy threads). Connecting then succeeds with the server long gone.
/// What a stopped server cannot do is answer its own token with `200`, which a stranger on its old
/// port never does.
#[must_use]
pub fn http_status(addr: SocketAddr, path: &str, token: &str) -> Option<u16> {
    use std::io::{Read, Write};
    let mut conn = TcpStream::connect_timeout(&addr, Duration::from_secs(1)).ok()?;
    conn.set_read_timeout(Some(Duration::from_secs(1))).ok()?;
    conn.set_write_timeout(Some(Duration::from_secs(1))).ok()?;
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {addr}\r\nAuthorization: Bearer {token}\r\nConnection: close\r\n\r\n"
    );
    conn.write_all(request.as_bytes()).ok()?;
    let mut head = Vec::new();
    let mut chunk = [0u8; 256];
    while !head.windows(2).any(|w| w == b"\r\n") && head.len() < 1024 {
        match conn.read(&mut chunk) {
            // A signal can cut a blocking read short (EINTR); the reply is still on its way.
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Ok(0) | Err(_) => break,
            Ok(n) => head.extend_from_slice(chunk.get(..n).unwrap_or_default()),
        }
    }
    let line = String::from_utf8_lossy(&head);
    let line = line.lines().next()?;
    let mut words = line.split(' ');
    words.next().filter(|v| v.starts_with("HTTP/1."))?;
    words.next()?.parse().ok()
}

fn has_a_peer(conn: &TcpStream) -> bool {
    matches!((conn.local_addr(), conn.peer_addr()), (Ok(local), Ok(peer)) if local != peer)
}

/// A temporary folder only the current account may enter, whatever the umask. msb refuses a home
/// (its catalog) with an ancestor that other accounts can write to, and under a umask of `0002` a
/// plain temporary folder is group-writable: a test that opens an msb home makes it in here.
///
/// # Panics
/// When the folder cannot be made.
#[must_use]
pub fn private_tempdir() -> tempfile::TempDir {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()
            .expect("make a private temporary folder for a test")
    }
    #[cfg(not(unix))]
    tempfile::tempdir().expect("make a private temporary folder for a test")
}

/// A temporary folder with a unique name that is removed when the value drops.
#[derive(Debug)]
pub struct TestDir(tempfile::TempDir);

impl TestDir {
    /// A new empty folder whose name starts with `label` (it shows in a failure that leaves the folder
    /// behind).
    ///
    /// # Panics
    /// When the folder cannot be made.
    #[must_use]
    pub fn new(label: &str) -> Self {
        Self(
            tempfile::Builder::new()
                .prefix(label)
                .tempdir()
                .expect("make a temporary folder for a test"),
        )
    }
}

impl Deref for TestDir {
    type Target = Path;
    fn deref(&self) -> &Path {
        self.0.path()
    }
}

impl AsRef<Path> for TestDir {
    fn as_ref(&self) -> &Path {
        self.0.path()
    }
}

#[cfg(test)]
mod tests {
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
}
