// SPDX-License-Identifier: AGPL-3.0-or-later
//! `puddle-soak run`: the Linux soak playbook's day with no person. It lays out the built
//! `puddle` as an install, starts `puddle serve` over a fresh data folder, runs the sampler alone
//! first, brings up the four workspaces and their loops (build, JavaScript, agent, web app), plays the schedule of events
//! ([`super::plan`]), idles through the night, runs the morning round, collects the artefacts,
//! tears down, and judges `metrics.tsv` ([`super::judge`]). The run fails on any MAJOR, the
//! judge's or its own (a crash outside the 5:00 event, a workspace that stopped by itself, a
//! morning that differs from hour 0, a process that outlived the hard kill).
//!
//! Next to the page loads of a working day the run opens the event stream once a playbook minute
//! and closes it again, as a window's page does on each load; a leak per connect therefore shows
//! in the resident size of `puddle serve` (the canary of `soak.yml` builds one on purpose).

use std::fmt::Write as _;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;
use tokio::process::{Child, Command};
use tokio::task::JoinHandle;

use super::judge::{Schedule, Severity, judge};
use super::plan::{Event, Profile, Workload, events, rounds};
use super::sample::{Config, Sampler, Target, descendants};
use super::series::Series;

/// What a run needs.
#[derive(Debug, Clone)]
pub struct Options {
    /// The built `puddle` program (with the `serve` feature).
    pub puddle: PathBuf,
    /// The msb runtime folder (`ci/fetch-msb.sh`); without it the run has no workspaces.
    pub runtime: Option<PathBuf>,
    /// The guest agent (`ci/build-agent.sh`), linked into the program's runtime folder.
    pub agent: Option<PathBuf>,
    /// The run folder: `metrics.tsv`, `run.log`, `serve.log`, `findings.txt`, `summary.md`, ...
    pub out: PathBuf,
    /// How long a playbook hour lasts.
    pub profile: Profile,
    /// The folder of the guest scripts (`ci/soak`); without it the run has no workloads.
    pub scripts: Option<PathBuf>,
    /// A command for the UI events, run with `PUDDLE_SOAK_URL`, `PUDDLE_SOAK_TOKEN` and
    /// `PUDDLE_SOAK_EVENT` set; a failure is a MAJOR finding.
    pub ui: Option<String>,
    /// The loopback port of the API; a fixed one so the address survives a restart.
    pub port: u16,
    /// Free memory the run needs at the start, GiB.
    pub min_ram_gib: u64,
    /// Free disk the run needs at the start in the run folder, GiB.
    pub min_disk_gib: u64,
}

impl Options {
    fn workloads(&self) -> bool {
        self.runtime.is_some() && self.scripts.is_some()
    }
}

/// One finding of the run itself (the judge's come from `metrics.tsv`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    /// MAJOR or MINOR.
    pub severity: Severity,
    /// What happened, with the numbers.
    pub text: String,
}

impl std::fmt::Display for Note {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let sev = match self.severity {
            Severity::Major => "MAJOR",
            Severity::Minor => "MINOR",
        };
        write!(f, "{sev} Run {}", self.text)
    }
}

/// A field of a JSON reply, `null` when it is missing.
fn at<'a>(v: &'a Value, key: &str) -> &'a Value {
    v.get(key).unwrap_or(&Value::Null)
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The run log: one line per step, with the UTC time and the playbook time.
#[derive(Clone)]
struct Log {
    file: Arc<Mutex<std::fs::File>>,
    start: Instant,
    profile: Profile,
}

impl Log {
    #[expect(
        clippy::print_stderr,
        reason = "the run's progress goes to the job log as well as run.log"
    )]
    fn line(&self, text: &str) {
        let real = self.start.elapsed().as_secs();
        let minutes = real * 60 / self.profile.hour_secs.max(1);
        let line = format!("{} {}:{:02} {text}", now(), minutes / 60, minutes % 60);
        eprintln!("{line}");
        if let Ok(mut f) = self.file.lock() {
            let _ = writeln!(f, "{line}");
        }
    }
}

/// One request to the API over HTTP/1.0 (never chunked), with a JSON body.
///
/// # Errors
/// A connection error, a timeout, or a reply that is not HTTP.
pub async fn call(
    api: &Target,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> io::Result<(u16, Vec<u8>)> {
    let body = body.map(Value::to_string).unwrap_or_default();
    let mut req = format!(
        "{method} {path} HTTP/1.0\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n",
        api.addr,
        body.len()
    );
    if let Some(token) = &api.token {
        let _ = write!(req, "Authorization: Bearer {token}\r\n");
    }
    req.push_str("\r\n");
    req.push_str(&body);
    let work = async {
        let mut stream = TcpStream::connect(&api.addr).await?;
        stream.write_all(req.as_bytes()).await?;
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).await?;
        Ok::<_, io::Error>(raw)
    };
    let raw = tokio::time::timeout(Duration::from_secs(300), work)
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "no reply in 300 s"))??;
    let bad = || io::Error::new(io::ErrorKind::InvalidData, "not an HTTP reply");
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(bad)?;
    let head = String::from_utf8_lossy(raw.get(..split).ok_or_else(bad)?).into_owned();
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(bad)?;
    Ok((status, raw.get(split + 4..).unwrap_or_default().to_vec()))
}

async fn call_json(api: &Target, method: &str, path: &str, body: Option<&Value>) -> (u16, Value) {
    match call(api, method, path, body).await {
        Ok((status, raw)) => (status, serde_json::from_slice(&raw).unwrap_or(Value::Null)),
        Err(e) => (0, json!({ "error": e.to_string() })),
    }
}

/// The run's `puddle serve`.
struct Serve {
    child: Child,
    api: Target,
}

/// A program folder laid out as an install: `puddle`, and `runtime` with a link to each file of
/// the msb runtime folder and to the guest agent.
fn program_folder(out: &Path, opts: &Options) -> io::Result<PathBuf> {
    let folder = out.join("program");
    let runtime = folder.join("runtime");
    std::fs::create_dir_all(&runtime)?;
    let exe = folder.join("puddle");
    std::fs::copy(&opts.puddle, &exe)?;
    let mut files: Vec<(PathBuf, std::ffi::OsString)> = Vec::new();
    if let Some(dir) = &opts.runtime {
        for entry in std::fs::read_dir(dir)?.flatten() {
            files.push((entry.path(), entry.file_name()));
        }
    }
    if let Some(agent) = &opts.agent {
        files.push((agent.clone(), "puddle-agent".into()));
    }
    for (from, name) in files {
        let link = runtime.join(name);
        if std::fs::symlink_metadata(&link).is_err() {
            std::os::unix::fs::symlink(std::path::absolute(from)?, link)?;
        }
    }
    Ok(exe)
}

async fn start_serve(exe: &Path, data: &Path, log: &Path, port: u16) -> Result<Serve, String> {
    let connection = data.join("api.json");
    let _ = std::fs::remove_file(&connection);
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
        .map_err(|e| format!("{}: {e}", log.display()))?;
    let err = file.try_clone().map_err(|e| e.to_string())?;
    let mut child = Command::new(exe)
        .args(["serve", "--headless", "--secret-store", "file", "--port"])
        .arg(port.to_string())
        .arg("--data-dir")
        .arg(data)
        .arg("--connection-file")
        .arg(&connection)
        .stdin(Stdio::null())
        .stdout(file)
        .stderr(err)
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("start puddle serve: {e}"))?;
    let deadline = Instant::now() + Duration::from_secs(120);
    // flake-ok: waits for the connection file a starting server writes, bounded by the deadline
    loop {
        if let Ok(Some(status)) = child.try_wait() {
            return Err(format!("puddle serve exited at start: {status}"));
        }
        if let Some(info) = std::fs::read(&connection)
            .ok()
            .and_then(|raw| serde_json::from_slice::<Value>(&raw).ok())
        {
            let url = at(&info, "url").as_str().unwrap_or_default();
            let addr = url
                .strip_prefix("http://")
                .unwrap_or(url)
                .trim_end_matches('/')
                .to_owned();
            let token = at(&info, "token").as_str().map(str::to_owned);
            return Ok(Serve {
                child,
                api: Target { addr, token },
            });
        }
        if Instant::now() > deadline {
            return Err("puddle serve wrote no connection file in 120 s".to_owned());
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// One workspace of the run.
#[derive(Debug, Clone)]
struct Space {
    name: &'static str,
    id: String,
}

/// The playbook's four workspaces; the last serves the web app on port 3000.
const SPACES: [&str; 4] = [
    "qa-soak-build",
    "qa-soak-js",
    "qa-soak-agent",
    "qa-soak-web",
];

/// The web app's port, in the guest and forwarded on this computer.
const WEB_PORT: u16 = 3000;

/// The hosts the loops need that no built-in set covers: the Rust installer's.
const OUTSIDE_THE_SETS: [&str; 2] = ["sh.rustup.rs", "static.rust-lang.org"];

/// The built-in sets the playbook switches on for all.
const SETS: [&str; 3] = ["package-registries", "debian-ubuntu", "github"];

struct Run {
    opts: Options,
    log: Log,
    data: PathBuf,
    exe: PathBuf,
    serve: Serve,
    sampler: Arc<tokio::sync::Mutex<Sampler>>,
    notes: Vec<Note>,
    spaces: Vec<Space>,
    loops: Vec<(Workload, JoinHandle<()>)>,
    loop_failures: Arc<Mutex<Vec<String>>>,
    crashes: u32,
}

impl Run {
    fn major(&mut self, text: String) {
        self.log.line(&format!("MAJOR {text}"));
        self.notes.push(Note {
            severity: Severity::Major,
            text,
        });
    }

    fn minor(&mut self, text: String) {
        self.log.line(&format!("MINOR {text}"));
        self.notes.push(Note {
            severity: Severity::Minor,
            text,
        });
    }

    fn api(&self) -> Target {
        self.serve.api.clone()
    }

    async fn restart(&mut self) -> Result<(), String> {
        let serve = start_serve(
            &self.exe,
            &self.data,
            &self.opts.out.join("serve.log"),
            self.opts.port,
        )
        .await?;
        {
            let sampler = self.sampler.lock().await;
            sampler.retarget(serve.api.clone());
            sampler.only_below(serve.child.id());
        }
        self.serve = serve;
        Ok(())
    }

    /// A `puddle serve` that ended outside a planned event is a crash: MAJOR, one restart; a
    /// second ends the run.
    async fn crash_watch(&mut self) -> Result<(), String> {
        let Ok(Some(status)) = self.serve.child.try_wait() else {
            return Ok(());
        };
        self.crashes += 1;
        self.major(format!(
            "puddle serve ended by itself ({status}); crash {} of the run",
            self.crashes
        ));
        if self.crashes > 1 {
            return Err("a second unplanned crash ends the run".to_owned());
        }
        self.restart().await?;
        self.start_spaces().await;
        Ok(())
    }

    async fn wait_status(&self, id: &str, want: &str, within: Duration) -> Result<(), String> {
        let deadline = Instant::now() + within;
        // flake-ok: waits for a workspace operation that takes minutes, bounded by the deadline
        loop {
            let (_, body) =
                call_json(&self.api(), "GET", &format!("/api/workspaces/{id}"), None).await;
            let status = at(&body, "status").as_str().unwrap_or_default().to_owned();
            if status == want {
                return Ok(());
            }
            if Instant::now() > deadline {
                return Err(format!(
                    "{id} is {status:?}, not {want} after {within:?}: {body}"
                ));
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    }

    async fn create_spaces(&mut self) -> Result<(), String> {
        for slug in SETS {
            let (code, body) = call_json(
                &self.api(),
                "PUT",
                &format!("/api/rule-sets/builtin:{slug}/switch"),
                Some(&json!({"enabled": true})),
            )
            .await;
            if code != 200 {
                return Err(format!("switch on {slug}: {code} {body}"));
            }
        }
        for host in OUTSIDE_THE_SETS {
            let rule = json!({"scope": {"type": "global"}, "pattern": host, "effect": "allow"});
            let (code, body) = call_json(&self.api(), "POST", "/api/rules", Some(&rule)).await;
            if code != 201 {
                return Err(format!("allow {host}: {code} {body}"));
            }
        }
        for name in SPACES {
            let (code, body) = call_json(
                &self.api(),
                "POST",
                "/api/workspaces",
                Some(&json!({"name": name, "memory_mib": 2048})),
            )
            .await;
            if code != 202 {
                return Err(format!("create {name}: {code} {body}"));
            }
            let id = at(&body, "id").as_str().unwrap_or_default().to_owned();
            self.spaces.push(Space { name, id });
        }
        for s in self.spaces.clone() {
            self.wait_status(&s.id, "running", Duration::from_mins(20))
                .await?;
            self.log.line(&format!("{} ({}) is running", s.name, s.id));
        }
        Ok(())
    }

    /// Starts every workspace that is not running (after a restart of Puddle) and waits for it.
    async fn start_spaces(&mut self) {
        for s in self.spaces.clone() {
            let (_, body) = call_json(
                &self.api(),
                "GET",
                &format!("/api/workspaces/{}", s.id),
                None,
            )
            .await;
            if at(&body, "status") == "running" {
                continue;
            }
            let (code, reply) = call_json(
                &self.api(),
                "POST",
                &format!("/api/workspaces/{}/start", s.id),
                None,
            )
            .await;
            if code != 202 {
                self.major(format!(
                    "start {} after the restart: {code} {reply}",
                    s.name
                ));
                continue;
            }
            if let Err(e) = self
                .wait_status(&s.id, "running", Duration::from_mins(10))
                .await
            {
                self.major(format!("{} did not come back: {e}", s.name));
            }
        }
        self.setup_web().await;
    }

    fn space(&self, name: &str) -> Option<Space> {
        self.spaces.iter().find(|s| s.name == name).cloned()
    }

    /// Runs `script` (a file of the scripts folder) with `args` in the workspace as the session
    /// user, as a terminal of the workspace does, and returns its exit code and output.
    async fn guest(&self, space: &Space, script: &str, args: &[String]) -> (i32, String) {
        guest(&self.opts, &self.data, space, script, args).await
    }

    async fn setup_web(&mut self) {
        let Some(web) = self.space("qa-soak-web") else {
            return;
        };
        let (code, out) = self.guest(&web, "web-setup.sh", &[]).await;
        if code != 0 {
            self.major(format!("the web app did not start: {out}"));
            return;
        }
        // Forwarded from this computer's port 3000, pinned so it stays over a restart; the agent
        // lists a new listener within seconds, so the forward is tried for a minute.
        let path = format!("/api/workspaces/{}/ports/{WEB_PORT}", web.id);
        let forward = json!({"host_port": WEB_PORT, "pinned": true});
        let mut last = (0, Value::Null);
        for _ in 0..12 {
            last = call_json(&self.api(), "PUT", &path, Some(&forward)).await;
            if last.0 == 200 {
                return;
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
        self.major(format!("forward of port {WEB_PORT}: {} {}", last.0, last.1));
    }

    async fn setup(&mut self) -> Result<(), String> {
        for s in self.spaces.clone() {
            let script = format!("{}-setup.sh", workload_tag(s.name));
            let (code, out) = self.guest(&s, &script, &[]).await;
            self.log.line(&format!("setup of {}: exit {code}", s.name));
            if code != 0 {
                return Err(format!("setup of {}: exit {code}\n{out}", s.name));
            }
        }
        Ok(())
    }

    fn start_round(&mut self, w: Workload, round: u64) {
        if let Some((_, h)) = self.loops.iter().find(|(x, h)| *x == w && !h.is_finished()) {
            let _ = h;
            self.log.line(&format!(
                "{w:?} round {round} skipped: the last one still runs"
            ));
            return;
        }
        self.loops.retain(|(x, _)| *x != w);
        let Some(space) = self.space(w.workspace()) else {
            return;
        };
        let (opts, data, log, failures) = (
            self.opts.clone(),
            self.data.clone(),
            self.log.clone(),
            self.loop_failures.clone(),
        );
        let handle = tokio::spawn(async move {
            let started = Instant::now();
            let (code, out) = guest(&opts, &data, &space, w.script(), &[round.to_string()]).await;
            let took = started.elapsed().as_secs();
            log.line(&format!("{w:?} round {round}: exit {code} in {took} s"));
            append(
                &opts.out.join("rounds.tsv"),
                &format!("{}\t{w:?}\t{round}\t{code}\t{took}\n", now()),
            );
            if code != 0 {
                let tail: String = out.lines().rev().take(20).collect::<Vec<_>>().join(" | ");
                failures
                    .lock()
                    .map(|mut f| f.push(format!("{w:?} round {round}: exit {code}: {tail}")))
                    .ok();
            }
        });
        self.loops.push((w, handle));
    }

    async fn stop_loops(&mut self) {
        for (w, h) in std::mem::take(&mut self.loops) {
            if !h.is_finished() {
                self.log.line(&format!("waiting for {w:?}'s last round"));
            }
            let _ = h.await;
        }
    }

    /// The Inbox once an hour: allow what the loops need, deny the stand-in's made-up hosts.
    async fn inbox_batch(&mut self) {
        let (code, body) = call_json(&self.api(), "GET", "/api/pending", None).await;
        if code != 200 {
            self.major(format!("GET /api/pending: {code} {body}"));
            return;
        }
        let rows = at(&body, "requests")
            .as_array()
            .cloned()
            .unwrap_or_default();
        let (mut allowed, mut denied) = (0, 0);
        for row in rows.iter().filter(|r| at(r, "state") == "pending") {
            let host = at(row, "host").as_str().unwrap_or_default();
            let verb = if host.starts_with("qa-soak-") {
                "deny"
            } else {
                "approve"
            };
            let (code, reply) = call_json(
                &self.api(),
                "POST",
                &format!("/api/pending/{}/{verb}", at(row, "id")),
                Some(&json!({})),
            )
            .await;
            if code != 200 {
                self.major(format!("{verb} {host}: {code} {reply}"));
            } else if verb == "deny" {
                denied += 1;
            } else {
                allowed += 1;
            }
        }
        self.log
            .line(&format!("inbox batch: {allowed} allowed, {denied} denied"));
    }

    async fn event(&mut self, e: Event) -> Result<(), String> {
        self.log.line(&format!("event {}", e.name()));
        match e {
            Event::Baseline | Event::StopLoops => {}
            Event::InboxBatch => self.inbox_batch().await,
            Event::RestartJs => {
                if let Some(s) = self.space("qa-soak-js") {
                    let (code, body) = call_json(
                        &self.api(),
                        "POST",
                        &format!("/api/workspaces/{}/stop", s.id),
                        None,
                    )
                    .await;
                    if code != 202 {
                        self.major(format!("stop the JavaScript workspace: {code} {body}"));
                    } else if let Err(e) = self
                        .wait_status(&s.id, "stopped", Duration::from_mins(5))
                        .await
                    {
                        self.major(format!("the JavaScript workspace did not stop: {e}"));
                    }
                    self.start_spaces().await;
                }
            }
            Event::RestartPuddle => {
                let pid = self.serve.child.id().unwrap_or_default();
                let _ = std::process::Command::new("kill")
                    .args(["-TERM", &pid.to_string()])
                    .status();
                match tokio::time::timeout(Duration::from_mins(2), self.serve.child.wait()).await {
                    Ok(Ok(status)) if status.success() => {}
                    other => self.major(format!("puddle serve did not quit cleanly: {other:?}")),
                }
                self.restart().await?;
                self.start_spaces().await;
            }
            Event::HardKill => {
                let pid = self.serve.child.id().unwrap_or_default();
                let started = descendants(pid);
                let _ = self.serve.child.kill().await;
                tokio::time::sleep(Duration::from_secs(5)).await;
                let left: Vec<u32> = started
                    .iter()
                    .copied()
                    .filter(|p| Path::new(&format!("/proc/{p}")).exists())
                    .collect();
                if !left.is_empty() {
                    self.major(format!("{} of {} processes of puddle serve outlived the hard kill by 5 s: {left:?}", left.len(), started.len()));
                }
                self.restart().await?;
                self.start_spaces().await;
            }
            Event::Backup => {
                let (code, raw) = call(&self.api(), "POST", "/api/backup", None)
                    .await
                    .unwrap_or((0, Vec::new()));
                if code != 200 || !raw.starts_with(&[0x1f, 0x8b]) {
                    self.major(format!(
                        "backup while the loops run: {code}, {} bytes",
                        raw.len()
                    ));
                } else {
                    self.log.line(&format!("backup: {} bytes", raw.len()));
                }
            }
            Event::Activity | Event::Rules | Event::Flood | Event::InboxKeys => self.ui(e).await,
        }
        Ok(())
    }

    async fn ui(&mut self, e: Event) {
        let Some(cmd) = self.opts.ui.clone() else {
            self.log
                .line(&format!("{}: no --ui command, skipped", e.name()));
            return;
        };
        let api = self.api();
        let out = Command::new("sh")
            .args(["-c", &cmd])
            .env("PUDDLE_SOAK_URL", format!("http://{}", api.addr))
            .env("PUDDLE_SOAK_TOKEN", api.token.unwrap_or_default())
            .env("PUDDLE_SOAK_EVENT", e.name())
            .env("PUDDLE_SOAK_OUT", &self.opts.out)
            .stdin(Stdio::null())
            .output()
            .await;
        match out {
            Ok(o) if o.status.success() => self.log.line(&format!("{}: passed", e.name())),
            Ok(o) => {
                let text = String::from_utf8_lossy(&o.stdout).into_owned()
                    + &String::from_utf8_lossy(&o.stderr);
                append(&self.opts.out.join(format!("ui-{}.log", e.name())), &text);
                self.major(format!(
                    "the {} event failed in the UI ({}); log ui-{}.log",
                    e.name(),
                    o.status,
                    e.name()
                ));
            }
            Err(err) => self.major(format!("the {} event could not run: {err}", e.name())),
        }
    }

    /// Hour 0 and the morning: the probe set and one build and one JavaScript round, as text to compare.
    async fn probes(&mut self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for s in self.spaces.clone() {
            let (code, text) = self.guest(&s, "probe.sh", &[]).await;
            out.push((s.name.to_owned(), format!("exit {code}\n{text}")));
        }
        out
    }

    async fn timed_round(&mut self, w: Workload, round: &str) -> Option<u64> {
        let space = self.space(w.workspace())?;
        let started = Instant::now();
        let (code, out) = self.guest(&space, w.script(), &[round.to_owned()]).await;
        if code != 0 {
            self.major(format!(
                "{w:?} {round} round failed: exit {code}: {}",
                out.lines().rev().take(10).collect::<Vec<_>>().join(" | ")
            ));
            return None;
        }
        Some(started.elapsed().as_secs())
    }
}

/// Appends a sample to `metrics` every `every`, for as long as the task lives.
async fn sample_into(sampler: Arc<tokio::sync::Mutex<Sampler>>, metrics: PathBuf, every: Duration) {
    // flake-ok: a sampler paced at a fixed interval for the length of the run
    loop {
        let mut s = Series::default();
        sampler.lock().await.sample(now(), &mut s).await;
        let mut lines = String::new();
        for l in s.render().lines().filter(|l| !l.starts_with('#')) {
            let _ = writeln!(lines, "{l}");
        }
        append(&metrics, &lines);
        tokio::time::sleep(every).await;
    }
}

/// Hour 0's probe results and build and JavaScript round times, for the morning to compare against.
#[derive(Debug, Default)]
struct Baseline {
    probes: Vec<(String, String)>,
    build: Option<u64>,
    js: Option<u64>,
}

impl Run {
    /// The loaded day: every event and loop round at its time. `None` when the run ended early
    /// (a second crash, the memory stop, a restart that failed).
    async fn day(&mut self, t0: Instant, api_slot: &Arc<Mutex<Target>>) -> Option<Baseline> {
        let (profile, workloads) = (self.opts.profile, self.opts.workloads());
        let mut baseline = Baseline::default();
        let mut timeline: Vec<(u64, Option<Event>, Option<Workload>)> = events(profile, workloads)
            .into_iter()
            .map(|(m, e)| (m, Some(e), None))
            .collect();
        if workloads {
            for w in Workload::ALL {
                timeline.extend(rounds(profile, w).into_iter().map(|m| (m, None, Some(w))));
            }
        }
        // Events first at the same minute: the baseline before the first rounds, a restart
        // before the rounds it would cut.
        timeline.sort_by_key(|&(m, e, _)| (m, e.is_none()));
        for (minute, event, workload) in timeline {
            tokio::time::sleep_until((t0 + profile.at(minute)).into()).await;
            if let Err(e) = self.crash_watch().await {
                self.major(e);
                return None;
            }
            if let Ok(mut slot) = api_slot.lock() {
                *slot = self.api();
            }
            // Under 4 GiB free memory the loops stop and the run ends 30 playbook minutes on.
            let meminfo = std::fs::read_to_string("/proc/meminfo").unwrap_or_default();
            if super::sample::available_ram_kb(&meminfo)
                .is_some_and(|kb| kb < 4.0 * 1024.0 * 1024.0)
            {
                self.major("free memory under 4 GiB: the loops stop (stop condition)".to_owned());
                self.stop_loops().await;
                tokio::time::sleep(profile.at(30)).await;
                return None;
            }
            match (event, workload) {
                (Some(Event::Baseline), _) if workloads => {
                    baseline.probes = self.probes().await;
                    baseline.build = self.timed_round(Workload::Build, "baseline").await;
                    baseline.js = self.timed_round(Workload::Js, "baseline").await;
                }
                (Some(Event::StopLoops), _) => self.stop_loops().await,
                (Some(e), _) => {
                    if let Err(err) = self.event(e).await {
                        self.major(err);
                        return None;
                    }
                }
                (None, Some(w)) => self.start_round(w, minute / w.period()),
                (None, None) => {}
            }
            let failed = std::mem::take(&mut *self.loop_failures.lock().unwrap());
            for f in failed {
                self.major(format!("a loop round failed: {f}"));
            }
        }
        Some(baseline)
    }

    /// The artefacts, every workspace deleted, `puddle serve` stopped.
    async fn teardown(&mut self) {
        collect(self).await;
        for s in self.spaces.clone() {
            let _ = call_json(
                &self.api(),
                "DELETE",
                &format!("/api/workspaces/{}", s.id),
                None,
            )
            .await;
        }
        let pid = self.serve.child.id().unwrap_or_default();
        let _ = std::process::Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .status();
        let _ = tokio::time::timeout(Duration::from_mins(2), self.serve.child.wait()).await;
        let _ = self.serve.child.kill().await;
    }

    /// The night until `end`: the sampler only; every workspace must stay up.
    async fn night(&mut self, end: Instant) {
        self.log.line("the night begins");
        let every = self.opts.profile.sample_every();
        // flake-ok: a watch over the night, paced by the sampler's period
        while Instant::now() < end {
            tokio::time::sleep(every.min(end.saturating_duration_since(Instant::now()))).await;
            if let Err(e) = self.crash_watch().await {
                self.major(e);
                return;
            }
            for s in self.spaces.clone() {
                let (_, body) = call_json(
                    &self.api(),
                    "GET",
                    &format!("/api/workspaces/{}", s.id),
                    None,
                )
                .await;
                if at(&body, "status") == "running" {
                    continue;
                }
                let (_, problems) = call_json(&self.api(), "GET", "/api/problems", None).await;
                let text = format!("{} stopped by itself in the night: {body}", s.name);
                if problems.to_string().contains(&s.id) {
                    self.minor(format!("{text} (with a notice)"));
                } else {
                    self.major(format!("{text}, and no notice says why"));
                }
                self.start_spaces().await;
            }
        }
    }

    /// The morning the same probe results as hour 0, build times within 20 %.
    async fn morning(&mut self, baseline: &Baseline) {
        self.log.line("the morning round");
        let probes = self.probes().await;
        for ((name, then), (_, now_text)) in baseline.probes.iter().zip(&probes) {
            if then != now_text {
                self.major(format!(
                    "{name}: the morning probes differ from hour 0:\n--- hour 0\n{then}\n--- morning\n{now_text}"
                ));
            }
        }
        for (w, then) in [
            (Workload::Build, baseline.build),
            (Workload::Js, baseline.js),
        ] {
            let now_secs = self.timed_round(w, "morning").await;
            if let (Some(a), Some(b)) = (then, now_secs)
                && b * 10 > a * 12
                && b > a + 5
            {
                self.major(format!(
                    "{w:?}'s morning round took {b} s against {a} s at hour 0 (line: 20 %)"
                ));
            }
        }
    }
}

/// The tag a workspace's scripts are named by: `build-setup.sh` for `qa-soak-build`, ...
fn workload_tag(space: &str) -> String {
    match space {
        "qa-soak-build" => "build",
        "qa-soak-js" => "js",
        "qa-soak-agent" => "agent",
        _ => "web",
    }
    .to_owned()
}

fn append(path: &Path, text: &str) {
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = f.write_all(text.as_bytes());
    }
}

async fn guest(
    opts: &Options,
    data: &Path,
    space: &Space,
    script: &str,
    args: &[String],
) -> (i32, String) {
    let (Some(runtime), Some(scripts)) = (&opts.runtime, &opts.scripts) else {
        return (127, "no runtime or scripts".to_owned());
    };
    let body = match std::fs::read_to_string(scripts.join(script)) {
        Ok(b) => b,
        Err(e) => return (127, format!("{script}: {e}")),
    };
    let home = data.join("msb");
    let quoted: Vec<String> = args
        .iter()
        .map(|a| format!("'{}'", a.replace('\'', "'\\''")))
        .collect();
    let inner = format!("cd && set -- {} && {}", quoted.join(" "), body);
    let out = Command::new(runtime.join("msb"))
        .args(["exec", &space.id, "--", "su", "-l", "agent", "-c", &inner])
        .env("MSB_HOME", &home)
        .env("MSB_CONFIG_PATH", home.join("config.json"))
        .env("MSB_PATH", runtime.join("msb"))
        .stdin(Stdio::null())
        .output()
        .await;
    match out {
        Ok(o) => (
            o.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&o.stdout).into_owned() + &String::from_utf8_lossy(&o.stderr),
        ),
        Err(e) => (-1, format!("msb exec: {e}")),
    }
}

/// Opens the event stream and closes it again once a playbook minute, as a page load does.
async fn page_loads(api: Arc<Mutex<Target>>, every: Duration) {
    // flake-ok: paced page loads for the length of the run, not a wait for a condition
    loop {
        let target = api.lock().map(|t| t.clone()).ok();
        if let Some(target) = target
            && let Ok(mut s) = TcpStream::connect(&target.addr).await
        {
            let req = format!(
                "GET /api/events HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {}\r\nAccept: text/event-stream\r\n\r\n",
                target.addr,
                target.token.unwrap_or_default()
            );
            if s.write_all(req.as_bytes()).await.is_ok() {
                let mut buf = [0_u8; 512];
                let _ = tokio::time::timeout(Duration::from_secs(5), s.read(&mut buf)).await;
            }
        }
        tokio::time::sleep(every).await;
    }
}

/// The memory and disk the run needs, from `/proc/meminfo` and `df`.
fn preflight(opts: &Options) -> Result<(), String> {
    let meminfo = std::fs::read_to_string("/proc/meminfo").map_err(|e| e.to_string())?;
    let ram_gib = super::sample::available_ram_kb(&meminfo).unwrap_or(0.0) / 1024.0 / 1024.0;
    let disk_gib = super::sample::free_disk_kb(&opts.out).unwrap_or(0.0) / 1024.0 / 1024.0;
    #[expect(clippy::cast_precision_loss, reason = "GiB counts are small")]
    let (want_ram, want_disk) = (opts.min_ram_gib as f64, opts.min_disk_gib as f64);
    if ram_gib < want_ram || disk_gib < want_disk {
        return Err(format!(
            "BLOCKED: {ram_gib:.1} GiB free memory (needs {want_ram}) and {disk_gib:.1} GiB free disk (needs {want_disk})"
        ));
    }
    Ok(())
}

/// The outcome of a run: the findings of the run and of the judge.
#[derive(Debug, Default)]
pub struct Outcome {
    /// Every finding, as the lines of `findings.txt`.
    pub lines: Vec<String>,
    /// Whether one is MAJOR.
    pub major: bool,
}

/// Runs the soak. `Err` is a run that could not be made (BLOCKED, a failed setup); the findings
/// of a run that was made are in the [`Outcome`].
///
/// # Errors
/// The reason the run could not be made.
pub async fn run(opts: Options) -> Result<Outcome, String> {
    std::fs::create_dir_all(&opts.out).map_err(|e| format!("{}: {e}", opts.out.display()))?;
    preflight(&opts)?;
    let log = Log {
        file: Arc::new(Mutex::new(
            std::fs::File::create(opts.out.join("run.log")).map_err(|e| e.to_string())?,
        )),
        start: Instant::now(),
        profile: opts.profile,
    };
    let data = opts.out.join("data");
    let exe = program_folder(&opts.out, &opts).map_err(|e| e.to_string())?;
    let serve = start_serve(&exe, &data, &opts.out.join("serve.log"), opts.port).await?;
    log.line(&format!("puddle serve is up on {}", serve.api.addr));
    let cfg = Config {
        api: serve.api.clone(),
        w4: opts.workloads().then(|| Target {
            addr: format!("127.0.0.1:{WEB_PORT}"),
            token: None,
        }),
        // `puddle$`: the server alone, not this `puddle-soak`.
        process_prefixes: vec!["puddle$".to_owned(), "msb".to_owned()],
        db: Some(data.join("puddle.db")),
        files: Vec::new(),
        disk: Some(opts.out.clone()),
    };
    let sampler = Sampler::start(cfg, now);
    // Only this run's server and what it started: another `puddle` on the machine is not ours.
    sampler.only_below(serve.child.id());
    let sampler = Arc::new(tokio::sync::Mutex::new(sampler));
    let metrics = opts.out.join("metrics.tsv");
    let every = opts.profile.sample_every();
    let sampling = tokio::spawn(sample_into(sampler.clone(), metrics.clone(), every));
    let mut run = Run {
        opts: opts.clone(),
        log: log.clone(),
        data,
        exe,
        serve,
        sampler: sampler.clone(),
        notes: Vec::new(),
        spaces: Vec::new(),
        loops: Vec::new(),
        loop_failures: Arc::new(Mutex::new(Vec::new())),
        crashes: 0,
    };

    // The sampler alone first; it must have written rows.
    tokio::time::sleep(opts.profile.sampler_alone()).await;
    let rows = std::fs::read_to_string(&metrics)
        .unwrap_or_default()
        .lines()
        .count();
    if rows == 0 {
        sampling.abort();
        return Err("the sampler wrote nothing while it ran alone".to_owned());
    }
    log.line(&format!("the sampler ran alone: {rows} rows"));

    if opts.workloads() {
        run.create_spaces().await?;
        run.setup().await?;
    }
    let api_slot = Arc::new(Mutex::new(run.api()));
    let loads = tokio::spawn(page_loads(api_slot.clone(), opts.profile.at(1)));

    // Hour 0 starts now: the judge's windows count from here.
    let start = now();
    let t0 = Instant::now();
    log.line(&format!("hour 0 at {start}"));
    let day = run.day(t0, &api_slot).await;
    if let Some(baseline) = day {
        run.night(t0 + opts.profile.active() + opts.profile.idle())
            .await;
        if opts.workloads() {
            run.morning(&baseline).await;
        }
    }

    // The artefacts, then down.
    loads.abort();
    sampling.abort();
    run.teardown().await;
    let schedule = Schedule {
        start,
        active_secs: opts.profile.active().as_secs(),
        hour: opts.profile.hour_secs,
    };
    verdict(&opts.out, &metrics, schedule, &run.notes)
}

/// The judge over `metrics.tsv` and the run's own findings, written to `findings.txt` and
/// `summary.md`.
fn verdict(
    out: &Path,
    metrics: &Path,
    schedule: Schedule,
    notes: &[Note],
) -> Result<Outcome, String> {
    let text = std::fs::read_to_string(metrics).unwrap_or_default();
    let series = Series::parse(&text).map_err(|e| e.to_string())?;
    let mut outcome = Outcome::default();
    for f in judge(&series, schedule) {
        outcome.major |= f.severity == Severity::Major;
        outcome.lines.push(f.to_string());
    }
    for n in notes {
        outcome.major |= n.severity == Severity::Major;
        outcome.lines.push(n.to_string());
    }
    let mut report = outcome.lines.join("\n");
    let _ = write!(
        report,
        "\n{} finding(s), {}\n",
        outcome.lines.len(),
        if outcome.major {
            "MAJOR: the run fails"
        } else {
            "no MAJOR: the run passes"
        }
    );
    std::fs::write(out.join("findings.txt"), &report).map_err(|e| e.to_string())?;
    std::fs::write(out.join("summary.md"), summary(&series, schedule, &report))
        .map_err(|e| e.to_string())?;
    Ok(outcome)
}

/// The audit log as JSON lines and the problems list, beside `serve.log` and `metrics.tsv`.
async fn collect(run: &Run) {
    let mut after: Option<i64> = None;
    let mut lines = String::new();
    for _ in 0..10_000 {
        let path = after.map_or_else(
            || "/api/audit?limit=500".to_owned(),
            |a| format!("/api/audit?limit=500&after={a}"),
        );
        let (code, body) = call_json(&run.api(), "GET", &path, None).await;
        let records = at(&body, "records").as_array().cloned().unwrap_or_default();
        if code != 200 || records.is_empty() {
            break;
        }
        for r in &records {
            let _ = writeln!(lines, "{r}");
        }
        match at(&body, "next_after").as_i64() {
            Some(n) if Some(n) != after => after = Some(n),
            _ => break,
        }
    }
    let _ = std::fs::write(run.opts.out.join("audit.jsonl"), lines);
    let (_, problems) = call_json(&run.api(), "GET", "/api/problems", None).await;
    let _ = std::fs::write(run.opts.out.join("problems.json"), problems.to_string());
}

/// The summary: the series of each Puddle process and call at hour 0, 4 and 8 and at the end.
fn summary(series: &Series, sched: Schedule, findings: &str) -> String {
    let hours = [0, 4, 8];
    let mut out = String::from(
        "# Soak run\n\n| metric | hour 0 | hour 4 | hour 8 | end |\n|---|---|---|---|---|\n",
    );
    let wanted = series
        .metrics("proc.", ".rss_kb")
        .chain(series.metrics("api.", ".ms"))
        .chain(series.metrics("file.", ".bytes"));
    for metric in wanted {
        let pts = series.points(metric);
        let at = |t: u64| {
            pts.iter()
                .find(|&&(pt, _)| pt >= t)
                .map_or_else(|| "-".to_owned(), |&(_, v)| format!("{v}"))
        };
        let cells: Vec<String> = hours
            .iter()
            .map(|h| at(sched.start + h * sched.hour))
            .collect();
        let end = pts
            .last()
            .map_or_else(|| "-".to_owned(), |&(_, v)| format!("{v}"));
        let _ = writeln!(out, "| {metric} | {} | {end} |", cells.join(" | "));
    }
    let _ = write!(out, "\n## Findings\n\n```\n{findings}```\n");
    out
}

#[cfg(test)]
mod tests;
