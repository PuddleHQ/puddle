// SPDX-License-Identifier: AGPL-3.0-or-later
//! Helpers the host tests share: a minimal HTTP client for the API and a reader of its event
//! stream, written on raw sockets so they depend on nothing the API's own tests do not.
#![expect(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "helpers outside #[test] functions fail the test by panicking; every slice is cut at an offset found in, or checked against, the same buffer"
)]

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
pub fn scratch_dir() -> tempfile::TempDir {
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
pub fn is_ram_mount(mount_point: &str) -> bool {
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
pub struct Api {
    addr: SocketAddr,
    token: String,
}

/// One answer: the status code and the body.
#[derive(Debug)]
pub struct Reply {
    pub status: u16,
    pub body: String,
}

impl Reply {
    pub fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or_else(|e| panic!("not JSON ({e}): {}", self.body))
    }
}

impl Api {
    pub fn new(url: &url::Url, token: &str) -> Self {
        let addr = SocketAddr::new(
            url.host_str().unwrap().parse().unwrap(),
            url.port().unwrap(),
        );
        Self {
            addr,
            token: token.to_owned(),
        }
    }

    pub async fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<&str>,
        token: bool,
    ) -> Reply {
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
        let head = format!(
            "{method} {path} HTTP/1.1\r\nHost: {}\r\n{auth}{content}Connection: close\r\n\r\n",
            self.addr
        );
        self.exchange(head.as_bytes(), body.as_bytes(), REQUEST_BOUND)
            .await
    }

    /// A `POST` whose body is `bytes` (a file), with the token.
    pub async fn post_bytes(&self, path: &str, bytes: &[u8]) -> Reply {
        let head = format!(
            "POST {path} HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {}\r\nContent-Type: application/gzip\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            self.addr,
            self.token,
            bytes.len()
        );
        self.exchange(head.as_bytes(), bytes, REQUEST_BOUND).await
    }

    /// Sends `head` and `body`, reads until the server closes, all within `within`; a panic
    /// names the failure only, as the request carries the token and stays out of the message.
    async fn exchange(&self, head: &[u8], body: &[u8], within: Duration) -> Reply {
        let run = async {
            let mut stream = TcpStream::connect(self.addr).await?;
            stream.write_all(head).await?;
            stream.write_all(body).await?;
            let mut raw = Vec::new();
            stream.read_to_end(&mut raw).await?;
            std::io::Result::Ok(raw)
        };
        let raw = tokio::time::timeout(within, run)
            .await
            .unwrap_or_else(|_| panic!("no complete answer within {within:?}"))
            .unwrap_or_else(|e| panic!("exchange failed: {e}"));
        parse_reply(&raw).unwrap_or_else(|e| panic!("exchange failed: {e}"))
    }

    pub async fn get(&self, path: &str) -> Reply {
        self.request("GET", path, None, true).await
    }

    pub async fn post(&self, path: &str, body: &str) -> Reply {
        self.request("POST", path, Some(body), true).await
    }

    pub async fn put(&self, path: &str, body: &str) -> Reply {
        self.request("PUT", path, Some(body), true).await
    }

    pub async fn patch(&self, path: &str, body: &str) -> Reply {
        self.request("PATCH", path, Some(body), true).await
    }

    pub async fn delete(&self, path: &str, body: Option<&str>) -> Reply {
        self.request("DELETE", path, body, true).await
    }

    /// Opens the event stream.
    pub async fn events(&self) -> Events {
        let mut stream = TcpStream::connect(self.addr).await.unwrap();
        let request = format!(
            "GET /api/events HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {}\r\nAccept: text/event-stream\r\n\r\n",
            self.addr, self.token
        );
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut events = Events::over(stream);
        // The response head tells the stream is open: events emitted from here on are seen.
        events
            .fill_until(
                |buf| find(buf, b"\r\n\r\n").is_some(),
                Duration::from_secs(10),
            )
            .await;
        let end = find(&events.buf, b"\r\n\r\n").unwrap();
        let head = String::from_utf8_lossy(&events.buf[..end]).into_owned();
        assert!(head.starts_with("HTTP/1.1 200"), "{head}");
        events.chunked = is_chunked(&head);
        let rest = events.buf.split_off(end + 4);
        events.buf.clear();
        events.absorb(&rest);
        events
    }
}

/// How long one API exchange may take, from connect to the server closing the connection.
const REQUEST_BOUND: Duration = Duration::from_secs(120);

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn is_chunked(head: &str) -> bool {
    head.to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
}

/// A whole answer read to the end of the connection: its status and its body, with a chunked
/// body reassembled. The split is made on bytes, so a body that is not UTF-8 cannot shift it.
fn parse_reply(raw: &[u8]) -> Result<Reply, String> {
    let end = find(raw, b"\r\n\r\n").ok_or_else(|| {
        format!(
            "the answer has no end of head: {:?}",
            String::from_utf8_lossy(raw)
        )
    })?;
    let head = String::from_utf8_lossy(&raw[..end]).into_owned();
    let status = head
        .split(' ')
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| format!("no status in {head:?}"))?;
    let body = &raw[end + 4..];
    let body = if is_chunked(&head) {
        dechunk(body)?
    } else {
        body.to_vec()
    };
    Ok(Reply {
        status,
        body: String::from_utf8_lossy(&body).into_owned(),
    })
}

/// The next whole chunk at the front of `raw`, removed from it: `Ok(None)` while it is still
/// incomplete, `Ok(Some(empty))` for the last chunk. A size that is not hexadecimal or a chunk
/// not followed by CRLF is an error, never a short body.
fn take_chunk(raw: &mut Vec<u8>) -> Result<Option<Vec<u8>>, String> {
    let Some(line_end) = find(raw, b"\r\n") else {
        return Ok(None);
    };
    let line = std::str::from_utf8(&raw[..line_end])
        .map_err(|_| format!("chunk size line is not text: {:?}", &raw[..line_end]))?;
    let size_text = line.split(';').next().unwrap_or_default().trim();
    let size = usize::from_str_radix(size_text, 16)
        .map_err(|e| format!("malformed chunk size {line:?}: {e}"))?;
    let data_start = line_end + 2;
    if size == 0 {
        raw.clear();
        return Ok(Some(Vec::new()));
    }
    let data_end = data_start + size;
    if raw.len() < data_end + 2 {
        return Ok(None);
    }
    if &raw[data_end..data_end + 2] != b"\r\n" {
        return Err(format!("chunk of {size} bytes is not followed by CRLF"));
    }
    let data = raw[data_start..data_end].to_vec();
    raw.drain(..data_end + 2);
    Ok(Some(data))
}

/// A chunked body, reassembled. A body that ends before its last chunk, or a malformed size,
/// is an error.
fn dechunk(body: &[u8]) -> Result<Vec<u8>, String> {
    let mut raw = body.to_vec();
    let mut out = Vec::new();
    loop {
        match take_chunk(&mut raw)? {
            Some(chunk) if chunk.is_empty() => return Ok(out),
            Some(chunk) => out.extend_from_slice(&chunk),
            None => {
                return Err(format!(
                    "chunked body ends before its last chunk ({} bytes read)",
                    out.len()
                ));
            }
        }
    }
}

/// The server-sent events of the API, parsed as they arrive.
pub struct Events {
    stream: TcpStream,
    /// Body bytes not yet part of a whole record.
    buf: Vec<u8>,
    /// Chunk-framed bytes not yet part of a whole chunk (chunked streams only).
    raw: Vec<u8>,
    chunked: bool,
    pub seen: Vec<Value>,
}

impl Events {
    /// Events read from an already connected stream whose response head is consumed.
    pub fn over(stream: TcpStream) -> Self {
        Self {
            stream,
            buf: Vec::new(),
            raw: Vec::new(),
            chunked: false,
            seen: Vec::new(),
        }
    }

    /// Adds bytes read from the socket to the body, unwrapping chunks when the stream is chunked.
    fn absorb(&mut self, bytes: &[u8]) {
        if !self.chunked {
            self.buf.extend_from_slice(bytes);
            return;
        }
        self.raw.extend_from_slice(bytes);
        while let Some(chunk) =
            take_chunk(&mut self.raw).unwrap_or_else(|e| panic!("event stream: {e}"))
        {
            self.buf.extend_from_slice(&chunk);
        }
    }

    /// Reads until `done` holds for the buffer; false when the deadline passed first. A read
    /// is tried even with no time left (`timeout` polls the read first), so what a stalled
    /// test thread finds already in the socket is not dropped.
    async fn try_fill_until(&mut self, done: impl Fn(&[u8]) -> bool, within: Duration) -> bool {
        let deadline = Instant::now() + within;
        let mut chunk = [0_u8; 4096];
        while !done(&self.buf) {
            let left = deadline.saturating_duration_since(Instant::now());
            match tokio::time::timeout(left, self.stream.read(&mut chunk)).await {
                Ok(Ok(0)) => panic!(
                    "the event stream ended; buffer: {:?}",
                    String::from_utf8_lossy(&self.buf)
                ),
                Ok(Ok(n)) => self.absorb(chunk.get(..n).unwrap()),
                Ok(Err(e)) => panic!("event stream: {e}"),
                Err(_) => return false,
            }
        }
        true
    }

    async fn fill_until(&mut self, done: impl Fn(&[u8]) -> bool, within: Duration) {
        assert!(
            self.try_fill_until(done, within).await,
            "timed out; buffer: {:?}",
            String::from_utf8_lossy(&self.buf)
        );
    }

    /// Reads events until one satisfies `pred`; returns it. Every event read is kept in `seen`.
    /// Records are decoded whole, so a character split across reads survives; a record whose
    /// data is not JSON panics, and a record with no data line (a keep-alive comment) is skipped.
    // flake-ok: reads the server-sent event stream (it consumes input, so a re-run probe cannot do it);
    // bounded by `within`, and the panic lists every event seen.
    pub async fn until(&mut self, pred: impl Fn(&Value) -> bool, within: Duration) -> Value {
        let deadline = Instant::now() + within;
        loop {
            while let Some(at) = find(&self.buf, b"\n\n") {
                let record: Vec<u8> = self.buf.drain(..at + 2).collect();
                if let Some(value) = parse_record(&record[..at]) {
                    self.seen.push(value.clone());
                    if pred(&value) {
                        return value;
                    }
                }
            }
            let left = deadline.saturating_duration_since(Instant::now());
            let before = self.buf.len();
            let filled = self
                .try_fill_until(|b| b.len() > before && find(b, b"\n\n").is_some(), left)
                .await;
            assert!(
                filled,
                "no matching event in {within:?}; saw {:#?}",
                self.seen
            );
        }
    }

    /// The workspace-progress steps seen so far for `workspace`, in order.
    pub fn steps(&self, workspace: &str) -> Vec<String> {
        self.seen
            .iter()
            .filter(|e| e["type"] == "workspace_progress" && e["workspace"] == workspace)
            .filter_map(|e| e["step"].as_str().map(str::to_owned))
            .collect()
    }
}

/// The JSON of one server-sent record, or `None` for a record with no data line. Panics on a
/// record that is not UTF-8 or whose data is not JSON: the stream broke its contract.
fn parse_record(record: &[u8]) -> Option<Value> {
    let text = std::str::from_utf8(record)
        .unwrap_or_else(|e| panic!("event record is not UTF-8 ({e}): {record:?}"));
    let data: Vec<&str> = text
        .lines()
        .filter_map(|l| l.strip_prefix("data:"))
        .map(str::trim_start)
        .collect();
    if data.is_empty() {
        return None;
    }
    let data = data.join("\n");
    Some(
        serde_json::from_str(&data).unwrap_or_else(|e| {
            panic!("event data is not JSON ({e}): {data:?} in record {text:?}")
        }),
    )
}

/// Matches the `workspace_progress` event of `workspace` that ends an operation.
pub fn ended(workspace: &str) -> impl Fn(&Value) -> bool + use<> {
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
pub fn capture_logs() -> Arc<std::sync::Mutex<Vec<u8>>> {
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
pub struct TestDevCert {
    folder: tempfile::TempDir,
    manager: DevCertManager<Unsupported>,
    given: AtomicBool,
    /// How many times the host asked for the certificate: a real one may open a system
    /// confirmation each time.
    asked: AtomicUsize,
}

impl TestDevCert {
    pub fn new() -> Self {
        let folder = scratch_dir();
        let manager = DevCertManager::new(folder.path(), Unsupported);
        Self {
            folder,
            manager,
            given: AtomicBool::new(false),
            asked: AtomicUsize::new(0),
        }
    }

    pub fn asked(&self) -> usize {
        self.asked.load(Ordering::SeqCst)
    }

    pub fn give(&self) {
        self.given.store(true, Ordering::SeqCst);
    }

    /// The next start gets a new leaf, as after a renewal.
    pub fn renew(&self) {
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

impl puddle_services::dev_cert::DevCertService for TestDevCert {
    fn status(&self) -> puddle_services::wire::DevCertificate {
        puddle_services::wire::DevCertificate::Unsupported
    }

    fn trust(&self) -> puddle_services::wire::DevCertTrustResult {
        puddle_services::wire::DevCertTrustResult {
            dev_certificate: puddle_services::wire::DevCertificate::Unsupported,
            detail: None,
            waiting: false,
        }
    }
}

/// A host that gives its guests no development certificate and reports none: the host's own
/// would read the machine's certificate stores and trust a certificate in them.
pub struct NoDevCert;

impl GuestCertSource for NoDevCert {
    fn guest_cert(&self) -> Option<GuestDevCert> {
        None
    }
}

impl puddle_services::dev_cert::DevCertService for NoDevCert {
    fn status(&self) -> puddle_services::wire::DevCertificate {
        puddle_services::wire::DevCertificate::NotChecked
    }

    fn trust(&self) -> puddle_services::wire::DevCertTrustResult {
        puddle_services::wire::DevCertTrustResult {
            dev_certificate: puddle_services::wire::DevCertificate::NotChecked,
            detail: None,
            waiting: false,
        }
    }
}

/// Options that touch nothing of this machine's. A workspace's id is drawn at random, as on a real
/// host, so a test that prints the id where the user reads the name fails. A test that must name
/// its paths before the workspace exists sets `workspace_ids` to [`puddle_services::workspaces::NameIds`].
pub fn hermetic_options() -> HostOptions {
    let mut options = HostOptions::default();
    options.dev_cert = Some(Arc::new(NoDevCert));
    options.free_space = Some(Arc::new(FixedSpace(FIXED_FREE_BYTES)));
    // No name resolves on this machine's DNS and no `az` runs: a resolver that is slow or down
    // must not decide how long a test takes.
    options.host_lookup = Some(puddle_upstream::HostLookup::new(|_, _| async {
        Err(std::io::Error::other("no names resolve in a hermetic host"))
    }));
    options.front_resolver = Some(Arc::new(puddle_api::NoNames));
    options.az_search_path = Some(std::ffi::OsString::new());
    options
}

/// [`hermetic_options`] with ids made from names (`acme` gets `acme`), for a test that writes
/// those ids into paths and URLs itself before the workspace exists. Every test that can read the
/// id from the reply uses [`hermetic_options`] instead.
pub fn named_id_options() -> HostOptions {
    let mut options = hermetic_options();
    options.workspace_ids = Some(Arc::new(puddle_services::workspaces::NameIds));
    options
}

/// The free space every hermetic host sees on its drive: a test about a grow never depends on
/// the machine it runs on.
pub const FIXED_FREE_BYTES: u64 = 512 * 1024 * 1024 * 1024;

/// A drive with a fixed figure of free space.
pub struct FixedSpace(pub u64);

impl puddle_host::Space for FixedSpace {
    fn free_bytes(&self) -> std::io::Result<u64> {
        Ok(self.0)
    }
}

#[cfg(test)]
mod tests;
