// SPDX-License-Identifier: AGPL-3.0-or-later
//! Helpers the host tests share: a minimal HTTP client for the API and a reader of its event
//! stream, written on raw sockets so they depend on nothing the API's own tests do not.
#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "helpers outside #[test] functions fail the test by panicking"
)]
#![allow(dead_code, reason = "each test binary uses some of the helpers")]

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use puddle_certs::devcert::{DevCertManager, GuestDevCert, LEAF_FILE, Unsupported};
use puddle_host::{GuestCertSource, HostOptions};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// A fresh folder for one test's data (the host's store, keys and settings).
///
/// The host fsyncs about fifteen files to create a workspace, so on a disk shared with heavy
/// writers (a build machine running several test runs at once) a test can sit in `fsync` for
/// minutes, longer than any wait of its own and than the runner's kill time. On Linux the
/// folder is therefore made in `/dev/shm` when that is its own RAM-backed, executable mount;
/// the test checks the host's logic, not the disk's durability, which the VM tests cover.
/// A `TMPDIR` the runner set is respected, and everywhere else this is `tempfile::tempdir()`.
pub(crate) fn scratch_dir() -> tempfile::TempDir {
    #[cfg(target_os = "linux")]
    if std::env::var_os("TMPDIR").is_none()
        && is_ram_mount("/dev/shm")
        && let Ok(dir) = tempfile::Builder::new()
            .prefix("puddle-test-")
            .tempdir_in("/dev/shm")
    {
        return dir;
    }
    tempfile::tempdir().unwrap()
}

/// Whether `mount_point` is a mount of its own with file system type `tmpfs` that is writable
/// and allows running programs, read from the kernel's mount table.
#[cfg(target_os = "linux")]
pub(crate) fn is_ram_mount(mount_point: &str) -> bool {
    let Ok(table) = std::fs::read_to_string("/proc/self/mountinfo") else {
        return false;
    };
    table.lines().any(|line| {
        // `id parent major:minor root mount-point options [optional...] - type source super`
        let (head, tail) = line.split_once(" - ").unwrap_or_default();
        let mut head = head.split(' ').skip(4);
        let (Some(at), Some(options)) = (head.next(), head.next()) else {
            return false;
        };
        let options: Vec<&str> = options.split(',').collect();
        at == mount_point
            && tail.split(' ').next() == Some("tmpfs")
            && options.contains(&"rw")
            && !options.contains(&"noexec")
    })
}

/// A client of one host's API.
pub(crate) struct Api {
    addr: SocketAddr,
    token: String,
}

/// One answer: the status code and the body.
pub(crate) struct Reply {
    pub status: u16,
    pub body: String,
}

impl Reply {
    pub(crate) fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or_else(|e| panic!("not JSON ({e}): {}", self.body))
    }
}

impl Api {
    pub(crate) fn new(url: &url::Url, token: &str) -> Self {
        let addr = SocketAddr::new(
            url.host_str().unwrap().parse().unwrap(),
            url.port().unwrap(),
        );
        Self {
            addr,
            token: token.to_owned(),
        }
    }

    pub(crate) async fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<&str>,
        token: bool,
    ) -> Reply {
        let mut stream = TcpStream::connect(self.addr).await.unwrap();
        let auth = if token {
            format!("Authorization: Bearer {}\r\n", self.token)
        } else {
            String::new()
        };
        let body = body.unwrap_or("");
        let content = if body.is_empty() {
            String::new()
        } else {
            format!(
                "Content-Type: application/json\r\nContent-Length: {}\r\n",
                body.len()
            )
        };
        let request = format!(
            "{method} {path} HTTP/1.1\r\nHost: {}\r\n{auth}{content}Connection: close\r\n\r\n{body}",
            self.addr
        );
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).await.unwrap();
        let text = String::from_utf8_lossy(&raw).into_owned();
        let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
        let status = head
            .split(' ')
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(|| panic!("no status in {head:?}"));
        Reply {
            status,
            body: dechunk(head, body),
        }
    }

    pub(crate) async fn get(&self, path: &str) -> Reply {
        self.request("GET", path, None, true).await
    }

    pub(crate) async fn post(&self, path: &str, body: &str) -> Reply {
        self.request("POST", path, Some(body), true).await
    }

    pub(crate) async fn put(&self, path: &str, body: &str) -> Reply {
        self.request("PUT", path, Some(body), true).await
    }

    pub(crate) async fn patch(&self, path: &str, body: &str) -> Reply {
        self.request("PATCH", path, Some(body), true).await
    }

    pub(crate) async fn delete(&self, path: &str, body: Option<&str>) -> Reply {
        self.request("DELETE", path, body, true).await
    }

    /// Opens the event stream.
    pub(crate) async fn events(&self) -> Events {
        let mut stream = TcpStream::connect(self.addr).await.unwrap();
        let request = format!(
            "GET /api/events HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {}\r\nAccept: text/event-stream\r\n\r\n",
            self.addr, self.token
        );
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut events = Events {
            stream,
            buf: String::new(),
            seen: Vec::new(),
        };
        // The response head tells the stream is open: events emitted from here on are seen.
        events
            .fill_until(|buf| buf.contains("\r\n\r\n"), Duration::from_secs(10))
            .await;
        assert!(events.buf.starts_with("HTTP/1.1 200"), "{}", events.buf);
        events.buf = events.buf.split_once("\r\n\r\n").unwrap().1.to_owned();
        events
    }
}

/// A chunked body, reassembled; other bodies unchanged.
fn dechunk(head: &str, body: &str) -> String {
    if !head
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        return body.to_owned();
    }
    let mut out = String::new();
    let mut rest = body;
    while let Some((size, tail)) = rest.split_once("\r\n") {
        let size = usize::from_str_radix(size.trim(), 16).unwrap_or(0);
        if size == 0 {
            break;
        }
        out.push_str(tail.get(..size).unwrap_or_default());
        rest = tail.get(size + 2..).unwrap_or_default();
    }
    out
}

/// The server-sent events of the API, parsed as they arrive.
pub(crate) struct Events {
    stream: TcpStream,
    buf: String,
    pub(crate) seen: Vec<Value>,
}

impl Events {
    /// Events read from an already connected stream.
    pub(crate) fn over(stream: TcpStream) -> Self {
        Self {
            stream,
            buf: String::new(),
            seen: Vec::new(),
        }
    }

    /// Reads until `done` holds for the buffer; false when the deadline passed first. A read
    /// is tried even with no time left (`timeout` polls the read first), so what a stalled
    /// test thread finds already in the socket is not dropped.
    async fn try_fill_until(&mut self, done: impl Fn(&str) -> bool, within: Duration) -> bool {
        let deadline = Instant::now() + within;
        let mut chunk = [0_u8; 4096];
        while !done(&self.buf) {
            let left = deadline.saturating_duration_since(Instant::now());
            match tokio::time::timeout(left, self.stream.read(&mut chunk)).await {
                Ok(Ok(0)) => panic!("the event stream ended; buffer: {:?}", self.buf),
                Ok(Ok(n)) => self
                    .buf
                    .push_str(&String::from_utf8_lossy(chunk.get(..n).unwrap())),
                Ok(Err(e)) => panic!("event stream: {e}"),
                Err(_) => return false,
            }
        }
        true
    }

    async fn fill_until(&mut self, done: impl Fn(&str) -> bool, within: Duration) {
        assert!(
            self.try_fill_until(done, within).await,
            "timed out; buffer: {:?}",
            self.buf
        );
    }

    /// Reads events until one satisfies `pred`; returns it. Every event read is kept in `seen`.
    // flake-ok: reads the server-sent event stream (it consumes input, so a re-run probe cannot do it);
    // bounded by `within`, and the panic lists every event seen.
    pub(crate) async fn until(&mut self, pred: impl Fn(&Value) -> bool, within: Duration) -> Value {
        let deadline = Instant::now() + within;
        loop {
            while let Some((record, rest)) = self.buf.split_once("\n\n") {
                let record = record.to_owned();
                self.buf = String::from(rest);
                let data: String = record
                    .lines()
                    .filter_map(|l| l.strip_prefix("data:"))
                    .map(str::trim_start)
                    .collect();
                if let Ok(value) = serde_json::from_str::<Value>(&data) {
                    self.seen.push(value.clone());
                    if pred(&value) {
                        return value;
                    }
                }
            }
            let left = deadline.saturating_duration_since(Instant::now());
            let before = self.buf.len();
            let filled = self
                .try_fill_until(|b| b.len() > before && b.contains("\n\n"), left)
                .await;
            assert!(
                filled,
                "no matching event in {within:?}; saw {:#?}",
                self.seen
            );
        }
    }

    /// The workspace-progress steps seen so far for `workspace`, in order.
    pub(crate) fn steps(&self, workspace: &str) -> Vec<String> {
        self.seen
            .iter()
            .filter(|e| e["type"] == "workspace_progress" && e["workspace"] == workspace)
            .filter_map(|e| e["step"].as_str().map(str::to_owned))
            .collect()
    }
}

/// Matches the `workspace_progress` event of `workspace` that ends an operation.
pub(crate) fn ended(workspace: &str) -> impl Fn(&Value) -> bool + use<> {
    let workspace = workspace.to_owned();
    move |e| {
        e["type"] == "workspace_progress"
            && e["workspace"] == workspace.as_str()
            && matches!(e["step"].as_str(), Some("done" | "failed"))
    }
}

/// Everything the process logs from here on, at every level, so a test can prove a value never
/// reached a log line. One subscriber per process: a test that needs it calls this before the
/// code under test runs, and the self-check fails loudly when another subscriber got there first.
pub(crate) fn capture_logs() -> Arc<std::sync::Mutex<Vec<u8>>> {
    use std::io::Write;
    use std::sync::{Arc, Mutex, OnceLock};

    struct Sink(Arc<Mutex<Vec<u8>>>);

    impl Write for Sink {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    static LOGS: OnceLock<Arc<Mutex<Vec<u8>>>> = OnceLock::new();
    let logs = LOGS.get_or_init(|| {
        let logs = Arc::new(Mutex::new(Vec::new()));
        let writer = Arc::clone(&logs);
        let installed = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .with_writer(move || Sink(Arc::clone(&writer)))
            .try_init()
            .is_ok();
        assert!(
            installed,
            "another log subscriber is installed in this process"
        );
        logs
    });
    tracing::info!("log capture is on");
    assert!(
        String::from_utf8_lossy(&logs.lock().unwrap()).contains("log capture is on"),
        "the capture does not see this test's logs"
    );
    Arc::clone(logs)
}

/// The development certificate the host gives its guests: Puddle's own leaf in a folder of the
/// test's, with no trust implementation, so nothing touches the machine's certificate stores.
/// Off until a test turns it on.
pub(crate) struct TestDevCert {
    folder: tempfile::TempDir,
    manager: DevCertManager<Unsupported>,
    given: AtomicBool,
    /// How many times the host asked for the certificate: a real one may open a system
    /// confirmation each time.
    asked: AtomicUsize,
}

impl TestDevCert {
    pub(crate) fn new() -> Self {
        let folder = scratch_dir();
        let manager = DevCertManager::new(folder.path(), Unsupported);
        Self {
            folder,
            manager,
            given: AtomicBool::new(false),
            asked: AtomicUsize::new(0),
        }
    }

    pub(crate) fn asked(&self) -> usize {
        self.asked.load(Ordering::SeqCst)
    }

    pub(crate) fn give(&self) {
        self.given.store(true, Ordering::SeqCst);
    }

    /// The next start gets a new leaf, as after a renewal.
    pub(crate) fn renew(&self) {
        let leaf = self.folder.path().join("dev-cert").join(LEAF_FILE);
        std::fs::remove_file(leaf).unwrap();
    }
}

impl GuestCertSource for TestDevCert {
    fn guest_cert(&self) -> Option<GuestDevCert> {
        self.asked.fetch_add(1, Ordering::SeqCst);
        self.given.load(Ordering::SeqCst).then(|| {
            let report = self.manager.ensure(std::time::SystemTime::now()).unwrap();
            GuestDevCert::new(&report.material)
        })
    }
}

impl puddle_api::DevCertService for TestDevCert {
    fn status(&self) -> puddle_api::wire::DevCertificate {
        puddle_api::wire::DevCertificate::Unsupported
    }

    fn trust(&self) -> puddle_api::wire::DevCertTrustResult {
        puddle_api::wire::DevCertTrustResult {
            dev_certificate: puddle_api::wire::DevCertificate::Unsupported,
            detail: None,
            waiting: false,
        }
    }
}

/// A host that gives its guests no development certificate and reports none: the host's own
/// would read the machine's certificate stores and trust a certificate in them.
pub(crate) struct NoDevCert;

impl GuestCertSource for NoDevCert {
    fn guest_cert(&self) -> Option<GuestDevCert> {
        None
    }
}

impl puddle_api::DevCertService for NoDevCert {
    fn status(&self) -> puddle_api::wire::DevCertificate {
        puddle_api::wire::DevCertificate::NotChecked
    }

    fn trust(&self) -> puddle_api::wire::DevCertTrustResult {
        puddle_api::wire::DevCertTrustResult {
            dev_certificate: puddle_api::wire::DevCertificate::NotChecked,
            detail: None,
            waiting: false,
        }
    }
}

/// Options that touch nothing of this machine's. A workspace's id is drawn at random, as on a real
/// host, so a test that prints the id where the user reads the name fails. A test that must name
/// its paths before the workspace exists sets `workspace_ids` to [`puddle_api::NameIds`].
pub(crate) fn hermetic_options() -> HostOptions {
    let mut options = HostOptions::default();
    options.dev_cert = Some(Arc::new(NoDevCert));
    options.free_space = Some(Arc::new(FixedSpace(FIXED_FREE_BYTES)));
    options
}

/// [`hermetic_options`] with ids made from names (`acme` gets `acme`), for a test that writes
/// those ids into paths and URLs itself before the workspace exists. Every test that can read the
/// id from the reply uses [`hermetic_options`] instead.
pub(crate) fn named_id_options() -> HostOptions {
    let mut options = hermetic_options();
    options.workspace_ids = Some(Arc::new(puddle_api::NameIds));
    options
}

/// The free space every hermetic host sees on its drive: a test about a grow never depends on
/// the machine it runs on.
pub(crate) const FIXED_FREE_BYTES: u64 = 512 * 1024 * 1024 * 1024;

/// A drive with a fixed figure of free space.
pub(crate) struct FixedSpace(pub(crate) u64);

impl puddle_host::Space for FixedSpace {
    fn free_bytes(&self) -> std::io::Result<u64> {
        Ok(self.0)
    }
}
