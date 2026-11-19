// SPDX-License-Identifier: AGPL-3.0-or-later
//! The soak sampler: what one sample of `metrics.tsv` holds and how each part is read. Linux
//! reads the host and processes from `/proc`; the Windows readings (handles, commit charge) are
//! added where that run lives. Every reader returns plain numbers, so the tests feed it files
//! and servers of their own.

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

use super::series::Series;

/// Longest a single request may take before it counts as failed.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// An API on a loopback address, with the bearer token the run uses.
#[derive(Debug, Clone)]
pub struct Target {
    /// `host:port`.
    pub addr: String,
    /// The token for `Authorization: Bearer`, if the API wants one.
    pub token: Option<String>,
}

fn request_text(target: &Target, path: &str) -> String {
    // HTTP/1.0 so the reply is never chunked and the body is what follows the headers.
    let mut req = format!("GET {path} HTTP/1.0\r\nHost: {}\r\n", target.addr);
    if let Some(token) = &target.token {
        let _ = write!(req, "Authorization: Bearer {token}\r\n");
    }
    req.push_str("\r\n");
    req
}

/// The reply to one GET.
#[derive(Debug)]
pub struct Reply {
    /// The status code.
    pub status: u16,
    /// What the body held.
    pub body: Vec<u8>,
    /// Time from connecting to the last byte.
    pub took: Duration,
}

/// One GET to `target`. Fails on a connection error, a timeout, or a reply with no status line.
///
/// # Errors
/// The I/O error, a timeout, or `InvalidData` for a reply that is not HTTP.
pub async fn get(target: &Target, path: &str) -> io::Result<Reply> {
    let started = Instant::now();
    let work = async {
        let mut stream = TcpStream::connect(&target.addr).await?;
        stream
            .write_all(request_text(target, path).as_bytes())
            .await?;
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).await?;
        Ok::<_, io::Error>(raw)
    };
    let raw = tokio::time::timeout(REQUEST_TIMEOUT, work)
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "no reply in 30 s"))??;
    let bad = |why: &'static str| io::Error::new(io::ErrorKind::InvalidData, why);
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| bad("no header end"))?;
    let head = String::from_utf8_lossy(raw.get(..split).ok_or_else(|| bad("no header"))?);
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| bad("no status line"))?;
    Ok(Reply {
        status,
        body: raw.get(split + 4..).unwrap_or_default().to_vec(),
        took: started.elapsed(),
    })
}

/// The four calls the playbook times, as metric name and path.
pub const API_CALLS: [(&str, &str); 4] = [
    ("health", "/api/health"),
    ("workspaces", "/api/workspaces"),
    ("pending", "/api/pending"),
    ("audit", "/api/audit?limit=200"),
];

/// The record counts the playbook reads: the length of a list, or the newest audit id.
///
/// # Errors
/// The body is not JSON of the expected shape.
pub fn count_in(body: &[u8], field: &str) -> Result<f64, String> {
    let json: serde_json::Value = serde_json::from_slice(body).map_err(|e| e.to_string())?;
    let value = json
        .get(field)
        .ok_or_else(|| format!("no `{field}` in the reply"))?;
    #[expect(
        clippy::cast_precision_loss,
        reason = "record counts are far below 2^52"
    )]
    match value {
        serde_json::Value::Array(a) => Ok(a.len() as f64),
        serde_json::Value::Number(n) => n.as_f64().ok_or_else(|| "not a number".to_owned()),
        _ => Err(format!("`{field}` is neither a list nor a number")),
    }
}

/// How the event stream has behaved: seconds since its last event, and reconnects.
#[derive(Debug)]
pub struct SseState {
    inner: Mutex<(Instant, u64)>,
}

impl SseState {
    /// The state at the moment the reader starts.
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new((Instant::now(), 0)),
        })
    }

    /// Seconds since the last event, and how many times the reader had to reconnect.
    #[must_use]
    pub fn read(&self) -> (f64, u64) {
        let g = self.inner.lock().unwrap();
        (g.0.elapsed().as_secs_f64(), g.1)
    }

    fn event(&self) {
        self.inner.lock().unwrap().0 = Instant::now();
    }

    fn reconnected(&self) {
        self.inner.lock().unwrap().1 += 1;
    }
}

/// Reads `GET /api/events` for ever, counting each `data:` or `event:` line as an event (a comment
/// line, the keep-alive, is not one) and reconnecting after a second when the stream ends. Ends
/// only when the task is dropped.
pub async fn read_events(target: Target, state: Arc<SseState>) {
    let mut first = true;
    loop {
        if !first {
            state.reconnected();
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        first = false;
        let Ok(mut stream) = TcpStream::connect(&target.addr).await else {
            continue;
        };
        if stream
            .write_all(request_text(&target, "/api/events").as_bytes())
            .await
            .is_err()
        {
            continue;
        }
        let mut lines = BufReader::new(stream).lines();
        let mut in_body = false;
        while let Ok(Some(line)) = lines.next_line().await {
            if !in_body {
                in_body = line.is_empty();
            } else if line.starts_with("data:") || line.starts_with("event:") {
                state.event();
            }
        }
    }
}

/// The last requests to the forwarded web app, so the share that failed in a window can be read.
#[derive(Debug, Default)]
pub struct W4Window {
    seen: VecDeque<(u64, bool)>,
}

impl W4Window {
    /// Notes the outcome of one request at `time` (epoch seconds).
    pub fn record(&mut self, time: u64, ok: bool) {
        self.seen.push_back((time, ok));
    }

    /// The share of the requests in the last `window` seconds that failed; `None` with no request
    /// in it. Older requests are dropped.
    pub fn fail_share(&mut self, now: u64, window: u64) -> Option<f64> {
        while self.seen.front().is_some_and(|&(t, _)| t + window <= now) {
            self.seen.pop_front();
        }
        #[expect(clippy::cast_precision_loss, reason = "a few dozen requests")]
        (!self.seen.is_empty()).then(|| {
            self.seen.iter().filter(|&&(_, ok)| !ok).count() as f64 / self.seen.len() as f64
        })
    }
}

/// Requests `path` from `target` every `every` and records the outcome (a 2xx or 3xx status is a
/// success). Ends only when the task is dropped.
pub async fn probe_web_app(
    target: Target,
    path: String,
    every: Duration,
    window: Arc<Mutex<W4Window>>,
    now: impl Fn() -> u64 + Send,
) {
    loop {
        let ok = get(&target, &path)
            .await
            .is_ok_and(|r| (200..400).contains(&r.status));
        window.lock().unwrap().record(now(), ok);
        tokio::time::sleep(every).await;
    }
}

/// RSS in KiB, threads and open file descriptors of one process, from `proc_dir` (`/proc/<pid>`).
#[must_use]
pub fn read_process(proc_dir: &Path) -> Option<[(&'static str, f64); 3]> {
    let status = std::fs::read_to_string(proc_dir.join("status")).ok()?;
    let field = |name: &str| -> Option<f64> {
        status.lines().find_map(|l| {
            l.strip_prefix(name)?
                .split_whitespace()
                .next()?
                .parse()
                .ok()
        })
    };
    let fds = std::fs::read_dir(proc_dir.join("fd")).ok()?.count();
    #[expect(clippy::cast_precision_loss, reason = "descriptor counts are small")]
    Some([
        ("rss_kb", field("VmRSS:")?),
        ("threads", field("Threads:")?),
        ("handles", fds as f64),
    ])
}

/// Every process under `proc_root` whose command name starts with one of `prefixes`, as
/// `(comm, pid)`.
#[must_use]
pub fn find_processes(proc_root: &Path, prefixes: &[String]) -> Vec<(String, u32)> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(proc_root) else {
        return found;
    };
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(comm) = std::fs::read_to_string(entry.path().join("comm")) else {
            continue;
        };
        let comm = comm.trim().to_owned();
        if prefixes.iter().any(|p| comm.starts_with(p.as_str())) {
            found.push((comm, pid));
        }
    }
    found.sort();
    found
}

/// Free RAM in KiB (`MemAvailable`) from the text of `/proc/meminfo`.
#[must_use]
pub fn available_ram_kb(meminfo: &str) -> Option<f64> {
    meminfo.lines().find_map(|l| {
        l.strip_prefix("MemAvailable:")?
            .split_whitespace()
            .next()?
            .parse()
            .ok()
    })
}

/// Busy and total jiffies from the first line of `/proc/stat`.
#[must_use]
pub fn cpu_jiffies(stat: &str) -> Option<(u64, u64)> {
    let nums: Vec<u64> = stat
        .lines()
        .next()?
        .strip_prefix("cpu")?
        .split_whitespace()
        .filter_map(|n| n.parse().ok())
        .collect();
    let total: u64 = nums.iter().take(8).sum();
    let idle = nums.get(3)? + nums.get(4).copied().unwrap_or(0);
    Some((total.saturating_sub(idle), total))
}

/// Free KiB on the file system holding `path`, from `df -Pk`.
#[must_use]
pub fn free_disk_kb(path: &Path) -> Option<f64> {
    let out = std::process::Command::new("df")
        .arg("-Pk")
        .arg(path)
        .output()
        .ok()?;
    parse_df(&String::from_utf8_lossy(&out.stdout))
}

fn parse_df(text: &str) -> Option<f64> {
    text.lines().nth(1)?.split_whitespace().nth(3)?.parse().ok()
}

/// What the sampler watches.
#[derive(Debug, Clone)]
pub struct Config {
    /// The API.
    pub api: Target,
    /// The web app of workload W4, when the run has one.
    pub w4: Option<Target>,
    /// Command-name prefixes of the processes to follow.
    pub process_prefixes: Vec<String>,
    /// `puddle.db`; its `-wal` is read too.
    pub db: Option<PathBuf>,
    /// Other data files, by the name that follows `file.` in the metric.
    pub files: Vec<(String, PathBuf)>,
    /// A path on the data drive, for free disk.
    pub disk: Option<PathBuf>,
}

/// Takes the samples of a run.
#[derive(Debug)]
pub struct Sampler {
    cfg: Config,
    sse: Arc<SseState>,
    w4: Arc<Mutex<W4Window>>,
    cpu_before: Option<(u64, u64)>,
}

/// Window the W4 failure share covers.
pub const W4_WINDOW_SECS: u64 = 300;

impl Sampler {
    /// A sampler with its event reader and web-app prober started on the current tokio runtime.
    #[must_use]
    pub fn start(cfg: Config, now: fn() -> u64) -> Self {
        let sse = SseState::new();
        tokio::spawn(read_events(cfg.api.clone(), sse.clone()));
        let w4 = Arc::new(Mutex::new(W4Window::default()));
        if let Some(target) = cfg.w4.clone() {
            tokio::spawn(probe_web_app(
                target,
                "/".to_owned(),
                Duration::from_secs(10),
                w4.clone(),
                now,
            ));
        }
        Self {
            cfg,
            sse,
            w4,
            cpu_before: None,
        }
    }

    /// One sample at `now`: every reading that works goes into `into`; one that fails is left out
    /// (a missing line is a gap in the series, never a made-up zero).
    pub async fn sample(&mut self, now: u64, into: &mut Series) {
        self.host(now, into);
        let procs = find_processes(Path::new("/proc"), &self.cfg.process_prefixes);
        for (comm, pid) in procs {
            if let Some(vals) = read_process(&Path::new("/proc").join(pid.to_string())) {
                for (field, v) in vals {
                    into.push(now, &format!("proc.{comm}.{pid}.{field}"), v);
                }
            }
        }
        self.files(now, into);
        self.api(now, into).await;
        let (age, reconnects) = self.sse.read();
        into.push(now, "sse.age_s", age);
        #[expect(clippy::cast_precision_loss, reason = "reconnect counts are tiny")]
        into.push(now, "sse.reconnects", reconnects as f64);
        if self.cfg.w4.is_some()
            && let Some(share) = self.w4.lock().unwrap().fail_share(now, W4_WINDOW_SECS)
        {
            into.push(now, "w4.fail_share", share);
        }
    }

    fn host(&mut self, now: u64, into: &mut Series) {
        if let Some(kb) = std::fs::read_to_string("/proc/meminfo")
            .ok()
            .and_then(|t| available_ram_kb(&t))
        {
            into.push(now, "host.free_ram_kb", kb);
        }
        let jiffies = std::fs::read_to_string("/proc/stat")
            .ok()
            .and_then(|t| cpu_jiffies(&t));
        if let (Some((b0, t0)), Some((b1, t1))) = (self.cpu_before, jiffies)
            && t1 > t0
        {
            #[expect(clippy::cast_precision_loss, reason = "jiffy counts")]
            into.push(
                now,
                "host.cpu_pct",
                100.0 * (b1 - b0) as f64 / (t1 - t0) as f64,
            );
        }
        self.cpu_before = jiffies;
        if let Some(kb) = self.cfg.disk.as_deref().and_then(free_disk_kb) {
            into.push(now, "host.free_disk_kb", kb);
        }
    }

    fn files(&self, now: u64, into: &mut Series) {
        let mut all: Vec<(String, PathBuf)> = self.cfg.files.clone();
        if let Some(db) = &self.cfg.db {
            all.push(("db".to_owned(), db.clone()));
            let mut wal = db.clone().into_os_string();
            wal.push("-wal");
            all.push(("db_wal".to_owned(), PathBuf::from(wal)));
        }
        for (name, path) in all {
            if let Ok(meta) = std::fs::metadata(&path) {
                #[expect(clippy::cast_precision_loss, reason = "file sizes below 2^52")]
                into.push(now, &format!("file.{name}.bytes"), meta.len() as f64);
            }
        }
    }

    async fn api(&self, now: u64, into: &mut Series) {
        for (name, path) in API_CALLS {
            let Ok(reply) = get(&self.cfg.api, path).await else {
                continue;
            };
            if reply.status != 200 {
                continue;
            }
            into.push(
                now,
                &format!("api.{name}.ms"),
                reply.took.as_secs_f64() * 1000.0,
            );
            let count = match name {
                "pending" => count_in(&reply.body, "requests").map(|c| ("pending", c)),
                "audit" => count_in(&reply.body, "next_after").map(|c| ("audit", c)),
                _ => continue,
            };
            if let Ok((what, c)) = count {
                into.push(now, &format!("count.{what}"), c);
            }
        }
        if let Ok(reply) = get(&self.cfg.api, "/api/problems").await
            && reply.status == 200
            && let Ok(c) = count_in(&reply.body, "problems")
        {
            into.push(now, "count.notices", c);
        }
    }
}

#[cfg(test)]
mod tests;
