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
/// The port is free when it is returned and not held, but no other test process is given it while
/// this one lives: a program that is not a test could still bind it.
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

/// The first free loopback port `first + k` for a `k` below `span` that no other test process has
/// claimed, tried from a start that differs per process and call.
///
/// A probe that binds and drops says the port is free now, not that it stays free until the caller
/// binds it, and the caller binds it a test setup later. Two test processes that pick the same
/// number in that gap both succeed and one of them loses the bind. So a pick is also claimed: an
/// exclusive lock on a file named after the port, in one folder every test process shares, held
/// until the process exits (the system drops it when a process dies, so a crash leaves nothing
/// stale).
fn free_port_in(first: u16, span: u16) -> Option<u16> {
    use std::sync::atomic::{AtomicU16, Ordering};
    static CALLS: AtomicU16 = AtomicU16::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    let start = (nanos ^ std::process::id().wrapping_mul(2_654_435_761)) % u32::from(span);
    let call = u32::from(CALLS.fetch_add(1, Ordering::Relaxed));
    free_port_from(first, span, start, call)
}

/// [`free_port_in`] with its starting point given: the same `start` and `call` in two processes
/// is the collision the claim is there for.
fn free_port_from(first: u16, span: u16, start: u32, call: u32) -> Option<u16> {
    (0..u32::from(span)).find_map(|step| {
        // 7919 is prime; for a span it does not divide, the steps visit every port once.
        let offset = (start + (call + step) * 7_919) % u32::from(span);
        let port = first + u16::try_from(offset).expect("the offset is below the span");
        let claim = claim(port)?;
        if TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], port))).is_err() {
            return None; // `claim` drops here and releases the number again
        }
        CLAIMS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(claim);
        Some(port)
    })
}

/// The claims this process holds, for as long as it lives.
static CLAIMS: std::sync::Mutex<Vec<std::fs::File>> = std::sync::Mutex::new(Vec::new());

/// The exclusive lock that makes `port` this process's, or `None` when it cannot be taken (another
/// process holds it; a lock that fails for another reason skips the port too, and a pick with no
/// port left fails loudly in the caller).
///
/// # Panics
/// When the claim folder or file cannot be made.
fn claim(port: u16) -> Option<std::fs::File> {
    use std::fs::File;
    #[expect(
        clippy::disallowed_methods,
        reason = "the folder must be the same one for every test process, so it cannot be a unique tempfile path"
    )]
    let temp = std::env::temp_dir();
    let dir = temp.join("puddle-test-ports");
    std::fs::create_dir_all(&dir).expect("make the folder test processes claim ports in");
    let file = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join(port.to_string()))
        .expect("open a port's claim file");
    file.try_lock().is_ok().then_some(file)
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
    use std::io::{BufRead, Read, Write};
    let mut conn = TcpStream::connect_timeout(&addr, Duration::from_secs(1)).ok()?;
    conn.set_read_timeout(Some(Duration::from_secs(1))).ok()?;
    conn.set_write_timeout(Some(Duration::from_secs(1))).ok()?;
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {addr}\r\nAuthorization: Bearer {token}\r\nConnection: close\r\n\r\n"
    );
    conn.write_all(request.as_bytes()).ok()?;
    let mut head = Vec::new();
    // `read_until` retries a read a signal cut short (EINTR) and keeps what it read when a read
    // fails or times out, so a partial status line still counts.
    let _ = std::io::BufReader::new((&conn).take(1024)).read_until(b'\n', &mut head);
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
mod tests;
