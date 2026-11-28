// SPDX-License-Identifier: AGPL-3.0-or-later
//! The soak sampler: what one sample of `metrics.tsv` holds and how each part is read. Linux
//! reads the host and processes from `/proc`; Windows reads them through [`super::winhost`]
//! (handle counts, working sets, commit charge). Every reader returns plain numbers, so the tests feed it files
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
#[cfg(windows)]
use super::winhost;

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

/// How the event stream has behaved: seconds since it last showed a sign of life (an event or a
/// keep-alive comment), and reconnects.
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

    /// Seconds since the last line of the stream, and how many times the reader had to reconnect.
    #[must_use]
    pub fn read(&self) -> (f64, u64) {
        let g = self.inner.lock().unwrap();
        (g.0.elapsed().as_secs_f64(), g.1)
    }

    fn alive(&self) {
        self.inner.lock().unwrap().0 = Instant::now();
    }

    fn reconnected(&self) {
        self.inner.lock().unwrap().1 += 1;
    }
}

async fn open_stream(target: &Target) -> io::Result<TcpStream> {
    let mut stream = TcpStream::connect(&target.addr).await?;
    stream
        .write_all(request_text(target, "/api/events").as_bytes())
        .await?;
    Ok(stream)
}

/// Where the API is now: a restart of `puddle serve` gives it a new token, and the soak run moves
/// its sampler there with [`Sampler::retarget`].
pub type ApiSlot = Arc<Mutex<Target>>;

/// Reads `GET /api/events` for ever, counting each line of the body as a sign of life (an event, or
/// the keep-alive comment an idle stream gets every 15 s: a quiet system sends no events, so only
/// a stream with neither is silent) and reconnecting after a second when the stream ends. Ends
/// only when the task is dropped.
pub async fn read_events(target: Target, state: Arc<SseState>) {
    read_events_at(Arc::new(Mutex::new(target)), state).await;
}

/// [`read_events`] against whatever `slot` holds at each connect.
pub async fn read_events_at(slot: ApiSlot, state: Arc<SseState>) {
    let mut first = true;
    // flake-ok: a reader that reconnects for ever until stopped, not a wait for a condition
    loop {
        if !first {
            state.reconnected();
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        first = false;
        let target = slot.lock().unwrap().clone();
        let Ok(stream) = open_stream(&target).await else {
            continue;
        };
        let mut lines = BufReader::new(stream).lines();
        let mut in_body = false;
        while let Ok(Some(line)) = lines.next_line().await {
            if !in_body {
                in_body = line.is_empty();
            } else if !line.is_empty() {
                state.alive();
            }
        }
    }
}

/// The last requests to the forwarded web app, so the share that failed in a window can be read.
#[derive(Debug, Default)]
pub struct W4Window {
    seen: VecDeque<(u64, bool)>,
    paused: bool,
}

impl W4Window {
    /// Notes the outcome of one request at `time` (epoch seconds); nothing while paused.
    pub fn record(&mut self, time: u64, ok: bool) {
        if !self.paused {
            self.seen.push_back((time, ok));
        }
    }

    /// Stops (or resumes) counting requests: a planned restart of the server leaves the forwarded
    /// port unanswered by design, so that gap is not a failure.
    pub fn pause(&mut self, paused: bool) {
        self.paused = paused;
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
    // flake-ok: a prober paced at a fixed interval until stopped, not a wait for a condition
    loop {
        let ok = get(&target, &path)
            .await
            .is_ok_and(|r| (200..400).contains(&r.status));
        window.lock().unwrap().record(now(), ok);
        tokio::time::sleep(every).await;
    }
}

/// Seconds the web app may take to answer again after a planned restart, counted from the moment
/// the workspaces are back; slower is a MAJOR finding.
pub const W4_RECOVERY_BOUND: Duration = Duration::from_secs(60);

/// Requests `path` from `target` every `poll` until it answers with a 2xx or 3xx status; the time
/// that took, or `None` when it still has not answered after `bound`.
pub async fn time_to_answer(
    target: &Target,
    path: &str,
    bound: Duration,
    poll: Duration,
) -> Option<Duration> {
    let start = Instant::now();
    // flake-ok: the run's own wait for the app after a restart, bounded by `bound`, not a test wait
    loop {
        if get(target, path)
            .await
            .is_ok_and(|r| (200..400).contains(&r.status))
        {
            return Some(start.elapsed());
        }
        if start.elapsed() >= bound {
            return None;
        }
        tokio::time::sleep(poll).await;
    }
}

/// RSS in KiB, threads and open file descriptors of one process, from `proc_dir` (`/proc/<pid>`).
/// The descriptors are left out when their folder cannot be read: `puddle serve` makes itself
/// non-dumpable, which gives `/proc/<pid>/fd` to root, while `status` stays readable.
#[must_use]
pub fn read_process(proc_dir: &Path) -> Option<Vec<(&'static str, f64)>> {
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
    let mut got = vec![
        ("rss_kb", field("VmRSS:")?),
        ("threads", field("Threads:")?),
    ];
    if let Ok(fds) = std::fs::read_dir(proc_dir.join("fd")) {
        #[expect(clippy::cast_precision_loss, reason = "descriptor counts are small")]
        got.push(("handles", fds.count() as f64));
    }
    Some(got)
}

/// Every process under `proc_root` whose command name starts with one of `prefixes`, as
/// `(comm, pid)`. A prefix that ends in `$` is a whole name: `puddle$` finds `puddle` and not
/// `puddle-soak`.
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
        if prefixes.iter().any(|p| {
            p.strip_suffix('$')
                .map_or_else(|| comm.starts_with(p.as_str()), |exact| comm == exact)
        }) {
            found.push((comm, pid));
        }
    }
    found.sort();
    found
}

/// Every process below `root` (children, their children, ...), from `/proc`; not `root` itself.
#[must_use]
pub fn descendants(root: u32) -> Vec<u32> {
    let mut parent_of = Vec::new();
    for entry in std::fs::read_dir("/proc").into_iter().flatten().flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|s| s.parse::<u32>().ok())
        else {
            continue;
        };
        let stat = std::fs::read_to_string(entry.path().join("stat")).unwrap_or_default();
        // The parent is the second field after the command, which is in parentheses.
        let parent = stat
            .rsplit_once(')')
            .and_then(|(_, rest)| rest.split_whitespace().nth(1))
            .and_then(|s| s.parse::<u32>().ok());
        if let Some(parent) = parent {
            parent_of.push((pid, parent));
        }
    }
    let mut found = vec![root];
    let mut i = 0;
    while let Some(&p) = found.get(i) {
        found.extend(
            parent_of
                .iter()
                .filter(|&&(_, pp)| pp == p)
                .map(|&(c, _)| c),
        );
        i += 1;
    }
    found.retain(|&p| p != root);
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
    /// The forwarded web app, when the run has one.
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
    api: ApiSlot,
    root: Arc<Mutex<Option<u32>>>,
    sse: Arc<SseState>,
    w4: Arc<Mutex<W4Window>>,
    w4_window_secs: u64,
    cpu_before: Option<(u64, u64)>,
}

/// Seconds the failed-request share covers when the caller names no other (5 playbook minutes of
/// the full profile).
pub const W4_WINDOW_SECS: u64 = 300;

/// Seconds between the web-app requests when the caller names no other.
pub const W4_EVERY: Duration = Duration::from_secs(10);

impl Sampler {
    /// A sampler with its event reader and web-app prober started on the current tokio runtime.
    #[must_use]
    pub fn start(cfg: Config, now: fn() -> u64) -> Self {
        let sse = SseState::new();
        let api = Arc::new(Mutex::new(cfg.api.clone()));
        tokio::spawn(read_events_at(api.clone(), sse.clone()));
        let w4 = Arc::new(Mutex::new(W4Window::default()));
        let mut sampler = Self {
            cfg,
            api,
            root: Arc::new(Mutex::new(None)),
            sse,
            w4,
            w4_window_secs: W4_WINDOW_SECS,
            cpu_before: None,
        };
        if let Some(target) = sampler.cfg.w4.clone() {
            sampler.watch_web_app(target, W4_EVERY, W4_WINDOW_SECS, now);
        }
        sampler
    }

    /// Starts requesting the web app every `every`; the share that failed covers `window_secs`.
    /// A run calls this once the app is up, so the time it took to start is not counted.
    pub fn watch_web_app(
        &mut self,
        target: Target,
        every: Duration,
        window_secs: u64,
        now: fn() -> u64,
    ) {
        self.w4_window_secs = window_secs;
        tokio::spawn(probe_web_app(
            target,
            "/".to_owned(),
            every,
            self.w4.clone(),
            now,
        ));
    }

    /// Stops counting the web-app requests (or counts them again) around a planned restart.
    pub fn w4_paused(&self, paused: bool) {
        self.w4.lock().unwrap().pause(paused);
    }

    /// After a planned restart: how long the web app takes to answer, or `None` past `bound`; a run
    /// that does not watch the web app has nothing to wait for (zero).
    pub async fn web_recovery(&self, bound: Duration) -> Option<Duration> {
        match &self.cfg.w4 {
            Some(target) => time_to_answer(target, "/", bound, Duration::from_secs(1)).await,
            None => Some(Duration::ZERO),
        }
    }

    /// Measures only the processes `root` is or started (from the next sample on), so another
    /// program of the same name on the machine is left out; a restart passes the new server.
    pub fn only_below(&self, root: Option<u32>) {
        *self.root.lock().unwrap() = root;
    }

    /// Points the API reads and the event reader at `api` from the next request or reconnect on.
    pub fn retarget(&self, api: Target) {
        *self.api.lock().unwrap() = api;
    }

    /// One sample at `now`: every reading that works goes into `into`; one that fails is left out
    /// (a missing line is a gap in the series, never a made-up zero).
    pub async fn sample(&mut self, now: u64, into: &mut Series) {
        self.host(now, into);
        self.processes(now, into);
        self.files(now, into);
        self.api(now, into).await;
        let (age, reconnects) = self.sse.read();
        into.push(now, "sse.age_s", age);
        #[expect(clippy::cast_precision_loss, reason = "reconnect counts are tiny")]
        into.push(now, "sse.reconnects", reconnects as f64);
        let window = self.w4_window_secs;
        let share = self.w4.lock().unwrap().fail_share(now, window);
        if let Some(share) = share {
            into.push(now, "w4.fail_share", share);
        }
    }

    /// The readings of every Puddle process: from `/proc`.
    #[cfg(not(windows))]
    fn processes(&self, now: u64, into: &mut Series) {
        let mut procs = find_processes(Path::new("/proc"), &self.cfg.process_prefixes);
        let root = *self.root.lock().unwrap();
        if let Some(root) = root {
            let ours = descendants(root);
            procs.retain(|&(_, pid)| pid == root || ours.contains(&pid));
        }
        for (comm, pid) in procs {
            let readings = read_process(&Path::new("/proc").join(pid.to_string()));
            for (field, v) in readings.into_iter().flatten() {
                into.push(now, &format!("proc.{comm}.{pid}.{field}"), v);
            }
        }
    }

    /// The readings of every Puddle process: working set, threads and handle count.
    #[cfg(windows)]
    fn processes(&self, now: u64, into: &mut Series) {
        let all = winhost::native::snapshot();
        let root = *self.root.lock().unwrap();
        for (comm, pid) in winhost::select(&all, &self.cfg.process_prefixes, root) {
            let Some((rss_kb, handles)) = winhost::native::process(pid) else {
                continue;
            };
            let threads = all.iter().find(|p| p.pid == pid).map_or(0, |p| p.threads);
            into.push(now, &format!("proc.{comm}.{pid}.rss_kb"), rss_kb);
            into.push(
                now,
                &format!("proc.{comm}.{pid}.threads"),
                f64::from(threads),
            );
            into.push(now, &format!("proc.{comm}.{pid}.handles"), handles);
        }
    }

    /// Free RAM, CPU and free disk of the machine, from `/proc` and `df`.
    #[cfg(not(windows))]
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

    /// Free RAM, commit charge and its limit, CPU and free disk of the machine.
    #[cfg(windows)]
    fn host(&mut self, now: u64, into: &mut Series) {
        for (name, v) in winhost::native::memory()
            .into_iter()
            .flat_map(winhost::Memory::readings)
        {
            into.push(now, &format!("host.{name}"), v);
        }
        let ticks = winhost::native::cpu_ticks();
        if let (Some((b0, t0)), Some((b1, t1))) = (self.cpu_before, ticks)
            && t1 > t0
        {
            #[expect(clippy::cast_precision_loss, reason = "tick counts")]
            into.push(
                now,
                "host.cpu_pct",
                100.0 * (b1 - b0) as f64 / (t1 - t0) as f64,
            );
        }
        self.cpu_before = ticks;
        if let Some(kb) = self
            .cfg
            .disk
            .as_deref()
            .and_then(winhost::native::free_disk_kb)
        {
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
        let api = self.api.lock().unwrap().clone();
        for (name, path) in API_CALLS {
            let Some(reply) = get(&api, path).await.ok().filter(|r| r.status == 200) else {
                continue;
            };
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
        if let Ok(reply) = get(&api, "/api/problems").await
            && reply.status == 200
            && let Ok(c) = count_in(&reply.body, "problems")
        {
            into.push(now, "count.notices", c);
        }
    }
}

#[cfg(test)]
mod tests;
