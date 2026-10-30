// SPDX-License-Identifier: GPL-3.0-or-later
//! The browser editor's server: the agent starts code-server on the guest's loopback and keeps it
//! running for as long as the agent runs.
//!
//! The host mounts the release read-only (`/puddle/code-server`) and tells the agent where
//! (`puddle_agent_proto::editor`). The agent
//!
//! - keeps the server's user data and extensions below the data folder, on the workspace volume,
//!   so they outlive a rebuilt sandbox;
//! - makes a random password for the sandbox's boot, writes it to an owner-only file in `/run` (the
//!   host reads it to sign a window in) and hands it to the server in its environment, never on its
//!   command line. A restarted server, or a restarted agent, keeps the password while the file
//!   holds one, so a window signed in stays signed in; a rebooted sandbox gets a new one;
//! - passes the reconnection grace on, and turns off the server's telemetry and update check
//!   (the release is replaced only with a Puddle release);
//! - starts it again when it exits, waiting longer after each quick exit. A server that an earlier
//!   agent left running (the agent was killed; the server outlives it) is left alone: the agent
//!   waits until the port is free instead of starting a second one that cannot bind.
//!
//! With the host's request for Microsoft's server ([`ServerKind::Microsoft`]) it runs that
//! instead, after downloading it ([`crate::microsoft`]); the restart rules, the password file (there
//! the server's connection token) and the port are the same.
//!
//! Microsoft's server is also kept up to date ([`Control`]): the agent looks for a newer build at
//! start and daily, downloads and verifies it beside the running one, and starts it only when the
//! host says no window is connected ([`Control::switch`]). The previous build is kept until the new
//! one has accepted a connection; a new build that exits first is dropped for it.
//!
//! The server runs without the `VSCODE_*` variables of the agent's environment: with
//! `VSCODE_IPC_HOOK_CLI` set, code-server hands its command line to the editor that variable
//! names instead of serving.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::FutureExt as _;
use puddle_agent_proto::AgentMessage;
use puddle_agent_proto::editor::{EditorServer, ServerKind};
use tokio::process::Command;
use tokio::sync::{Mutex, Notify, mpsc, watch};

use crate::microsoft::{self, Downloader, Fetch};

/// Where the server's user data lives, below [`EditorServer::data`].
const USER_DATA: &str = "user-data";
/// Where its extensions live, below [`EditorServer::data`].
const EXTENSIONS: &str = "extensions";
/// Its configuration file name, below [`EditorServer::data`] (it would default to `~/.config`).
const CONFIG: &str = "config.yaml";

/// A server that ran this long counts as healthy: the wait before a restart starts over.
const HEALTHY: Duration = Duration::from_secs(30);
/// How often the port of a server left by an earlier agent is looked at.
const PORT_POLL: Duration = Duration::from_secs(2);

/// How long [`has_a_peer`] waits for a connect attempt to settle.
const SETTLE: Duration = Duration::from_secs(1);
/// The longest wait between two starts.
const MAX_WAIT: Duration = Duration::from_secs(30);
/// The wait after the first refused download of Microsoft's server; each further one doubles it.
const REFUSED_WAIT: Duration = Duration::from_mins(1);
/// The longest wait between two downloads of a build that was refused.
const MAX_REFUSED_WAIT: Duration = Duration::from_hours(1);

/// How long a server is given to accept its first connection before it counts as not started.
const STARTUP: Duration = Duration::from_secs(120);
/// How long a server gets to end after `TERM` before it is killed.
const STOP_GRACE: Duration = Duration::from_secs(10);

/// When the agent looks for a newer build of Microsoft's server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpdateTimes {
    /// The wait before the first look, so a server that is starting is not slowed by it.
    pub first: Duration,
    /// The wait between two looks that worked.
    pub every: Duration,
    /// The wait before looking again after a look that failed (no network, a refused download).
    pub retry: Duration,
}

impl Default for UpdateTimes {
    fn default() -> Self {
        Self {
            first: Duration::from_secs(60),
            every: Duration::from_hours(24),
            retry: Duration::from_mins(15),
        }
    }
}

/// What the host's messages and the update schedule share with the running server: the builds
/// folder, the lock that keeps downloads and clean-ups apart, and the request to restart.
#[derive(Debug)]
pub struct Control {
    builds: PathBuf,
    /// Held by an install, a staging download and a clean-up: a clean-up during a download would
    /// remove the download's folders.
    work: Mutex<()>,
    restart: Notify,
    /// Held by whoever reads or moves the pointers (a switch, a claim of the build to run, a
    /// confirmation, a rollback), never across an await: those run in a few microseconds and must
    /// not interleave, or a build that is running could be deleted as "the previous one".
    pointers: std::sync::Mutex<()>,
    times: UpdateTimes,
    /// Whether this server updates at all (Microsoft's, with the user's setting on).
    updates: bool,
    /// Why the server cannot be installed, `None` when it can: every refused download sets it
    /// again (also to the same text), so a host that missed one hears the next.
    install_problem: watch::Sender<Option<String>>,
}

impl Control {
    /// The control of the server `server` runs.
    #[must_use]
    pub fn new(server: &EditorServer) -> Arc<Self> {
        Self::with_times(server, UpdateTimes::default())
    }

    /// [`Control::new`] with other waits (tests).
    #[must_use]
    pub fn with_times(server: &EditorServer, times: UpdateTimes) -> Arc<Self> {
        Arc::new(Self {
            builds: PathBuf::from(&server.dir),
            work: Mutex::new(()),
            restart: Notify::new(),
            pointers: std::sync::Mutex::new(()),
            times,
            install_problem: watch::channel(None).0,
            updates: matches!(
                server.kind,
                ServerKind::Microsoft {
                    auto_update: true,
                    ..
                }
            ),
        })
    }

    /// Runs the staged build, if there is one, by restarting the server: the host calls this when
    /// no window is connected. Returns the commit now current, `None` when nothing was staged.
    ///
    /// # Errors
    ///
    /// When the switch could not be recorded; the old build keeps running.
    pub fn switch(&self) -> io::Result<Option<String>> {
        if !self.updates {
            return Ok(None);
        }
        let _pointers = self.pointers_lock();
        let switched = microsoft::switch_to_staged(&self.builds)?;
        if switched.is_some() {
            self.restart.notify_one();
        }
        Ok(switched)
    }
}

impl Control {
    /// Follows why the server cannot be installed ([`forward_install_problem`] sends it to the
    /// host). Subscribe before the server runs: a receiver sees only what is set after it exists.
    #[must_use]
    pub fn install_problem(&self) -> watch::Receiver<Option<String>> {
        self.install_problem.subscribe()
    }

    /// A download was fetched and refused: say why, every time.
    fn install_refused(&self, reason: &str) {
        self.install_problem.send_replace(Some(reason.to_owned()));
    }

    /// The server got past its install: ends what [`Control::install_refused`] said, once.
    fn install_worked(&self) {
        self.install_problem.send_if_modified(|problem| {
            let was_set = problem.is_some();
            *problem = None;
            was_set
        });
    }

    fn pointers_lock(&self) -> std::sync::MutexGuard<'_, ()> {
        // Nothing under the lock panics; a poisoned lock is as good as a free one.
        self.pointers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The build to run now and whether it is on trial, read together with the restart request
    /// they answer: a switch before this is the build returned, one after it restarts the server.
    fn claim(&self) -> Option<(PathBuf, bool)> {
        let _pointers = self.pointers_lock();
        // A request left over from before is answered by this very read.
        let _ = self.restart.notified().now_or_never();
        let build = microsoft::current_build(&self.builds)?;
        Some((build, microsoft::on_trial(&self.builds)))
    }
}

/// Bytes of randomness in the password (hex: twice as many characters).
const PASSWORD_BYTES: usize = 32;

/// The editor server's settings and the file its password goes to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorConfig {
    /// How to run it.
    pub server: EditorServer,
    /// Where the password is written.
    pub password_file: PathBuf,
}

/// A fresh random password, lowercase hex.
///
/// # Errors
///
/// When the system has no randomness (the server must not start with a guessable password).
pub fn new_password() -> io::Result<String> {
    use std::fmt::Write as _;
    let mut bytes = [0_u8; PASSWORD_BYTES];
    getrandom::fill(&mut bytes).map_err(|e| io::Error::other(format!("no randomness: {e}")))?;
    Ok(bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}"); // writing to a String can't fail
        s
    }))
}

/// The password already in `path`, when it is one this module wrote.
fn existing_password(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let valid = text.len() == PASSWORD_BYTES * 2 && text.bytes().all(|b| b.is_ascii_hexdigit());
    valid.then_some(text)
}

/// Writes `password` to `path` for its owner only, through a temporary file renamed over it, so a
/// reader never sees half of it or an older file's mode.
///
/// # Errors
///
/// When the folder or the file can't be written.
pub fn write_password(path: &Path, password: &str) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut partial = path.as_os_str().to_owned();
    partial.push(".partial");
    let partial = PathBuf::from(partial);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    {
        use std::io::Write as _;
        let mut file = options.open(&partial)?;
        file.write_all(password.as_bytes())?;
    }
    std::fs::rename(&partial, path)
}

/// Most of the multi-root file that is read to see whether it lists a folder.
const WORKSPACE_FILE_PEEK: u64 = 1 << 20;

/// Whether `file` is a multi-root file that lists at least one folder. A safety net, not a parser
/// (the file is JSON with comments the user may have edited): it asks for the `folders` key and a
/// `path` entry anywhere, so a settings-only file, which the host leaves behind when it takes its
/// folders out, does not open an empty window. It cannot see a `path` that belongs to another key.
fn lists_a_folder(file: &Path) -> bool {
    use std::io::Read as _;
    let mut text = String::new();
    let read = std::fs::File::open(file)
        .and_then(|f| f.take(WORKSPACE_FILE_PEEK).read_to_string(&mut text));
    read.is_ok() && text.contains("\"folders\"") && text.contains("\"path\"")
}

/// What the server opens, looked at each time it starts: the multi-root file when there is one
/// that lists a folder, else the folder.
#[must_use]
pub fn target(server: &EditorServer) -> OsString {
    match &server.workspace_file {
        Some(file) if lists_a_folder(Path::new(file)) => OsString::from(file),
        _ => OsString::from(&server.folder),
    }
}

/// The command line of the server (without the program): the release's own flags, the target last.
#[must_use]
pub fn arguments(server: &EditorServer) -> Vec<OsString> {
    let data = Path::new(&server.data);
    let mut args: Vec<OsString> = vec![
        "--bind-addr".into(),
        format!("127.0.0.1:{}", server.port).into(),
        "--auth".into(),
        "password".into(),
        "--user-data-dir".into(),
        data.join(USER_DATA).into(),
        "--extensions-dir".into(),
        data.join(EXTENSIONS).into(),
        "--config".into(),
        data.join(CONFIG).into(),
        "--reconnection-grace-time".into(),
        server.grace_secs.to_string().into(),
        "--disable-telemetry".into(),
        "--disable-update-check".into(),
    ];
    args.push(target(server));
    args
}

/// Whether the server must not inherit the variable `name`: editor hand-over variables, our own
/// settings, and `PORT` (which code-server reads in place of its bind address).
fn withheld(name: &str) -> bool {
    name.starts_with("VSCODE_") || name.starts_with("PUDDLE_AGENT_") || name == "PORT"
}

/// Why the server could not be made ready.
enum NotReady {
    /// A download was fetched and refused (usually a wrong hash, or an archive that is not
    /// unpacked): the build is as wrong the next time, so the retry waits [`wait_after_refusal`].
    Refused {
        /// The error, for the log.
        reason: String,
        /// What the user is told ([`microsoft::InstallError::refusal_summary`]).
        summary: String,
    },
    Other(String),
}

/// What to start: the program, its arguments, and the password for its environment (code-server
/// reads it there; Microsoft's server reads its token from a file named in the arguments).
struct Launch {
    program: PathBuf,
    args: Vec<OsString>,
    password_env: bool,
    /// Microsoft's build this runs: its commit, and whether it has yet to prove itself.
    build: Option<(String, bool)>,
}

/// The server's command: `launch`, the password in its environment when it takes one, and none
/// of the variables in `inherited` that [`withheld`] names.
fn command(
    launch: &Launch,
    password: &str,
    inherited: impl IntoIterator<Item = String>,
) -> Command {
    let mut command = Command::new(&launch.program);
    command
        .args(&launch.args)
        .stdin(Stdio::null())
        .kill_on_drop(true);
    if launch.password_env {
        command.env("PASSWORD", password);
    }
    for name in inherited.into_iter().filter(|n| withheld(n)) {
        command.env_remove(name);
    }
    command
}

/// What the agent starts for `config`: code-server from the mounted release, or Microsoft's
/// server from the build on the volume (fetched first when there is none).
async fn launch(
    config: &EditorConfig,
    fetch: &impl Fetch,
    control: &Control,
) -> Result<Launch, NotReady> {
    let server = &config.server;
    match server.kind {
        ServerKind::CodeServer => Ok(Launch {
            program: Path::new(&server.dir).join("bin/code-server"),
            args: arguments(server),
            password_env: true,
            build: None,
        }),
        ServerKind::Microsoft { telemetry, .. } => {
            microsoft::prepare(server).map_err(|e| NotReady::Other(e.to_string()))?;
            let platform = microsoft::platform(std::env::consts::ARCH)
                .map_err(|e| NotReady::Other(e.to_string()))?;
            if microsoft::current_build(&control.builds).is_none() {
                // Not while an update download runs: it may be fetching this very build.
                let _work = control.work.lock().await;
                microsoft::ensure_build(fetch, platform, &control.builds)
                    .await
                    .map_err(|e| {
                        let reason = e.to_string();
                        if e.is_refusal() {
                            NotReady::Refused {
                                reason,
                                summary: e.refusal_summary(),
                            }
                        } else {
                            NotReady::Other(reason)
                        }
                    })?;
            }
            // The build to run, read together with the restart request it answers.
            let (build, on_trial) = control.claim().ok_or_else(|| {
                NotReady::Other("the editor server build vanished from the volume".to_owned())
            })?;
            let commit = build
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            Ok(Launch {
                build: Some((commit, on_trial)),
                program: build.join(microsoft::PROGRAM),
                args: microsoft::arguments(
                    server,
                    telemetry,
                    &config.password_file,
                    &target(server),
                ),
                password_env: false,
            })
        }
    }
}

/// The wait before start number `quick` (the number of quick exits in a row, 1 for the first).
#[must_use]
pub fn wait_after(quick: u32) -> Duration {
    Duration::from_secs(u64::from(quick.min(30)))
        .max(Duration::from_secs(1))
        .min(MAX_WAIT)
}

/// The wait before download number `refused` (the number of refused downloads in a row, 1 for the
/// first): a minute, doubling up to an hour.
#[must_use]
pub fn wait_after_refusal(refused: u32) -> Duration {
    REFUSED_WAIT
        .saturating_mul(2_u32.saturating_pow(refused.saturating_sub(1).min(16)))
        .min(MAX_REFUSED_WAIT)
}

/// Makes the data folders. The server makes the rest.
fn prepare(server: &EditorServer) -> io::Result<()> {
    for sub in [USER_DATA, EXTENSIONS] {
        std::fs::create_dir_all(Path::new(&server.data).join(sub))?;
    }
    Ok(())
}

/// Runs the server and starts it again whenever it exits; never returns.
pub async fn run(config: EditorConfig, control: Arc<Control>) {
    run_controlled(config, Downloader::system(), control).await;
}

/// [`run`], with `fetch` for what Microsoft's server needs from the network and a control of its
/// own.
pub async fn run_with(config: EditorConfig, fetch: impl Fetch) {
    let control = Control::new(&config.server);
    run_controlled(config, fetch, control).await;
}

/// [`run_with`], with the `control` the host's messages reach.
pub async fn run_controlled(config: EditorConfig, fetch: impl Fetch, control: Arc<Control>) {
    let fetch = Arc::new(fetch);
    let updating = control
        .updates
        .then(|| tokio::spawn(update_loop(Arc::clone(&control), Arc::clone(&fetch))));
    serve_forever(&config, &*fetch, &control).await;
    if let Some(updating) = updating {
        updating.abort();
    }
}

/// Sends every change of `problem` (a [`Control::install_problem`]) to the host as an
/// [`AgentMessage::EditorStatus`]. Returns when `out` closes.
pub async fn forward_install_problem(
    mut problem: watch::Receiver<Option<String>>,
    out: mpsc::Sender<AgentMessage>,
) {
    while problem.changed().await.is_ok() {
        let message = AgentMessage::editor_status(problem.borrow_and_update().as_deref());
        if out.send(message).await.is_err() {
            return;
        }
    }
}

/// Looks for a newer build now and then, and stages what it finds. Never returns.
async fn update_loop(control: Arc<Control>, fetch: Arc<impl Fetch>) {
    let Ok(platform) = microsoft::platform(std::env::consts::ARCH) else {
        return;
    };
    tokio::time::sleep(control.times.first).await;
    let mut refused = 0_u32;
    loop {
        let looked = {
            let _work = control.work.lock().await;
            microsoft::stage_update(&*fetch, platform, &control.builds).await
        };
        let is_ok = looked.is_ok();
        let wait = match looked {
            Ok(microsoft::Update::UpToDate) => {
                tracing::debug!("the editor server is the newest build");
                control.times.every
            }
            Ok(microsoft::Update::Staged(commit)) => {
                tracing::info!(%commit, "a newer editor server build is ready for the next quiet moment");
                control.times.every
            }
            Err(err) if err.is_refusal() => {
                refused = refused.saturating_add(1);
                tracing::warn!(%err, refused, "a newer editor server build was refused");
                wait_after_refusal(refused).max(control.times.retry)
            }
            Err(err) => {
                tracing::warn!(%err, "could not look for a newer editor server build");
                control.times.retry
            }
        };
        if is_ok {
            refused = 0;
        }
        tokio::time::sleep(wait).await;
    }
}

async fn serve_forever(config: &EditorConfig, fetch: &impl Fetch, control: &Arc<Control>) {
    let server = &config.server;
    if server.kind == ServerKind::CodeServer {
        let program = Path::new(&server.dir).join("bin/code-server");
        if !program.is_file() {
            // A sandbox made before the host bundled the server has no such mount; it gets one
            // when it is made again.
            tracing::warn!(program = %program.display(), "no editor server in this sandbox: it was made without the code-server mount");
            return;
        }
    }
    let mut quick = 0_u32;
    let mut refused = 0_u32;
    loop {
        let launched = launch(config, fetch, control).await;
        let started = Instant::now();
        match launched {
            Ok(launch) => {
                control.install_worked();
                match start_once(config, &launch, control).await {
                    Ok(status) => tracing::warn!(%status, "the editor server exited"),
                    Err(err) => tracing::error!(error = %err, "the editor server could not run"),
                }
            }
            // A refused download is retried rarely: the build is as wrong next time, and each try
            // downloads hundreds of MB.
            Err(NotReady::Refused { reason, summary }) => {
                refused = refused.saturating_add(1);
                let wait = wait_after_refusal(refused);
                tracing::error!(%reason, ?wait, refused, "the editor server download was refused; trying again later");
                control.install_refused(&summary);
                // A restart request (a build staged and switched to) ends the wait early.
                tokio::select! {
                    () = tokio::time::sleep(wait) => {}
                    () = control.restart.notified() => {}
                }
                continue;
            }
            // Counted as a quick exit: a missing network must not be retried every second.
            Err(NotReady::Other(reason)) => {
                tracing::error!(%reason, "the editor server could not be made ready");
                quick = quick.saturating_add(1);
                tokio::time::sleep(wait_after(quick)).await;
                continue;
            }
        }
        refused = 0;
        quick = if started.elapsed() >= HEALTHY {
            1
        } else {
            quick.saturating_add(1)
        };
        let wait = wait_after(quick);
        tracing::info!(
            port = server.port,
            ?wait,
            "starting the editor server again"
        );
        tokio::time::sleep(wait).await;
    }
}

/// Waits for the server on `port` to accept a connection (it is then a build worth keeping) and
/// records that. Gives up after [`STARTUP`].
async fn confirm_when_listening(control: Arc<Control>, port: u16, commit: String) {
    let deadline = Instant::now() + STARTUP;
    while Instant::now() < deadline {
        if port_taken(port).await {
            let _work = control.work.lock().await;
            let _pointers = control.pointers_lock();
            if let Err(err) = microsoft::confirm(&control.builds, &commit) {
                tracing::warn!(%err, "could not record that the editor server build started");
            }
            return;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

/// After a server exited: a build that exited before it ever accepted a connection is dropped
/// for the build it replaced.
async fn settle_trial(control: &Control, commit: &str) {
    let _work = control.work.lock().await;
    let _pointers = control.pointers_lock();
    match microsoft::roll_back(&control.builds, commit) {
        Ok(true) => tracing::error!(
            "the new editor server build exited before it started; running the previous one again"
        ),
        Ok(false) => {}
        Err(err) => tracing::warn!(%err, "could not go back to the previous editor server build"),
    }
}

/// Ends `child` with `TERM` (so the server ends what it started), `KILL` after [`STOP_GRACE`].
async fn stop(child: &mut tokio::process::Child) -> io::Result<std::process::ExitStatus> {
    #[cfg(target_os = "linux")]
    if let Some(pid) = child.id().and_then(|p| i32::try_from(p).ok()) {
        use nix::sys::signal::{Signal, kill};
        use nix::unistd::Pid;
        // Gone already is as good as ended.
        let _ = kill(Pid::from_raw(pid), Signal::SIGTERM);
    }
    if let Ok(status) = tokio::time::timeout(STOP_GRACE, child.wait()).await {
        status
    } else {
        child.start_kill()?;
        child.wait().await
    }
}

/// Whether something already accepts connections on the server's port.
async fn port_taken(port: u16) -> bool {
    match tokio::net::TcpStream::connect(("127.0.0.1", port)).await {
        Ok(stream) => has_a_peer(&stream).await,
        Err(_) => false,
    }
}

/// Whether a connect that reported success reached a listener. `Ok` is not enough on loopback: the
/// kernel joins a socket to itself when its source port happens to be the one it dials (nothing
/// listens there, so the port is free), and tokio can report `Ok` for an attempt that is still in
/// flight and then refused. Both look like a taken port, which delays the start by [`PORT_POLL`]
/// (or, while confirming a build, records a build that never listened). An attempt still unsettled
/// after [`SETTLE`] counts as taken, as every `Ok` did before.
async fn has_a_peer(stream: &tokio::net::TcpStream) -> bool {
    let deadline = Instant::now() + SETTLE;
    loop {
        match (stream.peer_addr(), stream.local_addr()) {
            (Ok(peer), Ok(local)) => return peer != local,
            (Err(err), _)
                if err.kind() == io::ErrorKind::NotConnected
                    && stream.take_error().ok().flatten().is_none() =>
            {
                if Instant::now() >= deadline {
                    return true;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            _ => return false,
        }
    }
}

/// Returns once `taken` says nothing answers on `port` (the probe is asked every [`PORT_POLL`]).
async fn wait_until_free<Fut>(port: u16, mut taken: impl FnMut(u16) -> Fut)
where
    Fut: Future<Output = bool>,
{
    let mut warned = false;
    while taken(port).await {
        if !warned {
            tracing::warn!(
                port,
                "the editor port is taken (by a server an earlier agent started?); waiting for it to be free"
            );
            warned = true;
        }
        tokio::time::sleep(PORT_POLL).await;
    }
}

async fn start_once(
    config: &EditorConfig,
    launch: &Launch,
    control: &Arc<Control>,
) -> io::Result<std::process::ExitStatus> {
    if config.server.kind == ServerKind::CodeServer {
        prepare(&config.server)?;
    }
    wait_until_free(config.server.port, port_taken).await;
    let password = if let Some(password) = existing_password(&config.password_file) {
        password
    } else {
        let password = new_password()?;
        write_password(&config.password_file, &password)?;
        password
    };
    let inherited = std::env::vars_os().filter_map(|(k, _)| k.into_string().ok());
    let mut child = command(launch, &password, inherited).spawn()?;
    tracing::info!(
        port = config.server.port,
        grace_secs = config.server.grace_secs,
        "the editor server started"
    );
    let confirming = launch.build.as_ref().map(|(commit, _)| {
        tokio::spawn(confirm_when_listening(
            Arc::clone(control),
            config.server.port,
            commit.clone(),
        ))
    });
    let mut replaced = false;
    let status = tokio::select! {
        status = child.wait() => status,
        () = control.restart.notified() => {
            tracing::info!("stopping the editor server to run its newer build");
            replaced = true;
            stop(&mut child).await
        }
    };
    if let Some(confirming) = confirming {
        confirming.abort();
    }
    // A build that ended on its own before it accepted a connection is not worth keeping; one
    // that was stopped to make room for the next is not a failure.
    if let Some((commit, true)) = &launch.build
        && !replaced
    {
        settle_trial(control, commit).await;
    }
    status
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server() -> EditorServer {
        EditorServer::new("/workspaces/a/.puddle/code-server", "/workspaces/a", 300)
    }

    #[test]
    fn the_command_line_binds_loopback_keeps_data_on_the_volume_and_never_holds_the_password() {
        let args: Vec<String> = arguments(&server())
            .into_iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let after = |flag: &str| {
            let at = args.iter().position(|a| a == flag).unwrap();
            args[at + 1].as_str()
        };
        assert_eq!(after("--bind-addr"), "127.0.0.1:41337");
        assert_eq!(after("--auth"), "password");
        // Built with `Path::join` like the code: the agent runs in the guest, but its unit tests
        // also run on a Windows host, where the separator differs.
        let under =
            |base: &str, leaf: &str| Path::new(base).join(leaf).to_string_lossy().into_owned();
        let data = "/workspaces/a/.puddle/code-server";
        assert_eq!(after("--user-data-dir"), under(data, "user-data"));
        assert_eq!(after("--extensions-dir"), under(data, "extensions"));
        assert_eq!(after("--config"), under(data, "config.yaml"));
        assert_eq!(after("--reconnection-grace-time"), "300");
        assert!(args.iter().any(|a| a == "--disable-telemetry"));
        assert!(args.iter().any(|a| a == "--disable-update-check"));
        assert_eq!(args.last().map(String::as_str), Some("/workspaces/a"));
        assert!(
            !args
                .iter()
                .any(|a| a.contains("password") && a != "password")
        );
    }

    #[test]
    fn the_server_opens_the_multi_root_file_only_while_it_lists_a_folder() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.code-workspace");
        let server = EditorServer::new("/d", "/workspaces/a", 300)
            .with_workspace_file(file.to_string_lossy());
        let opens = || target(&server).to_string_lossy().into_owned();
        // No file yet, a file of settings alone, a list with no folder in it: the folder.
        assert_eq!(opens(), "/workspaces/a");
        for text in [
            r#"{"settings": {"path": 1}}"#,
            "",
            r#"{"folders": []}"#,
            "not json at all",
        ] {
            std::fs::write(&file, text).unwrap();
            assert_eq!(opens(), "/workspaces/a", "{text:?}");
        }
        // A list with a folder, comments and all: the file, which is the last argument.
        std::fs::write(
            &file,
            "// kept\n{\"folders\": [{\"path\": \"/workspaces/a/api\"}], \"settings\": {}}",
        )
        .unwrap();
        assert_eq!(opens(), file.to_string_lossy());
        assert_eq!(
            arguments(&server)
                .last()
                .map(|a| a.to_string_lossy().into_owned()),
            Some(file.to_string_lossy().into_owned())
        );
        // A server told of no file never looks.
        let plain = EditorServer::new("/d", "/workspaces/a", 300);
        assert_eq!(target(&plain), OsString::from("/workspaces/a"));
    }

    /// A probe that says "taken" until `free` is set, counting how often it was asked.
    fn probe(
        asked: Arc<std::sync::atomic::AtomicUsize>,
        free: Arc<std::sync::atomic::AtomicBool>,
    ) -> impl FnMut(u16) -> std::future::Ready<bool> {
        use std::sync::atomic::Ordering::SeqCst;
        move |_| {
            asked.fetch_add(1, SeqCst);
            std::future::ready(!free.load(SeqCst))
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_taken_port_is_asked_about_again_and_again_until_it_is_free() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
        let (asked, free) = (
            Arc::new(AtomicUsize::new(0)),
            Arc::new(AtomicBool::new(false)),
        );
        let waiting = tokio::spawn(wait_until_free(
            41337,
            probe(Arc::clone(&asked), Arc::clone(&free)),
        ));
        // Ten poll intervals with the port taken: still waiting, and it kept asking.
        for _ in 0..10 {
            tokio::time::advance(PORT_POLL).await;
            tokio::task::yield_now().await;
        }
        assert!(
            !waiting.is_finished(),
            "it went on while the port was taken"
        );
        assert!(
            asked.load(SeqCst) >= 10,
            "asked {} times",
            asked.load(SeqCst)
        );
        free.store(true, SeqCst);
        tokio::time::advance(PORT_POLL).await;
        tokio::time::timeout(PORT_POLL, waiting)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn a_port_with_a_listener_is_taken_and_a_free_one_is_not() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(port_taken(port).await);
        drop(listener);
        assert!(!port_taken(port).await);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_connect_that_reached_only_itself_is_not_a_taken_port() {
        // The kernel's simultaneous open: bind a port, then connect to that same port with nothing
        // listening. The connect succeeds, to nobody.
        let probe = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = probe.local_addr().unwrap();
        drop(probe);
        let socket = tokio::net::TcpSocket::new_v4().unwrap();
        socket.set_reuseaddr(true).unwrap();
        socket.bind(addr).unwrap();
        let joined = socket.connect(addr).await.unwrap();
        assert_eq!(joined.local_addr().unwrap(), joined.peer_addr().unwrap());
        assert!(!has_a_peer(&joined).await);
        // A real connection to a listener has one.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let real = tokio::net::TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        assert!(has_a_peer(&real).await);
    }

    #[test]
    fn passwords_are_long_hex_and_different_every_time() {
        let a = new_password().unwrap();
        let b = new_password().unwrap();
        assert_eq!(a.len(), PASSWORD_BYTES * 2);
        assert!(a.bytes().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    #[test]
    fn only_a_password_this_module_wrote_is_reused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pw");
        assert_eq!(existing_password(&path), None);
        let good = new_password().unwrap();
        for (case, bad) in [
            "",
            "short",
            &"g".repeat(64),
            &format!("{good}\n"),
            &format!("{good}00"),
        ]
        .into_iter()
        .enumerate()
        {
            std::fs::write(&path, bad).unwrap();
            assert_eq!(
                existing_password(&path),
                None,
                "a malformed password file (case {case}) was reused"
            );
        }
        std::fs::write(&path, &good).unwrap();
        assert_eq!(existing_password(&path), Some(good));
    }

    #[test]
    fn the_password_file_is_owner_only_and_replaced_whole() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("run/editor-password");
        let first = puddle_types::fixture::password("first");
        let second = puddle_types::fixture::password("second");
        write_password(&path, &first).unwrap();
        write_password(&path, &second).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), second);
        assert!(!dir.path().join("run/editor-password.partial").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    fn the_server_gets_the_password_in_its_environment_and_none_of_the_editor_hand_over_variables()
    {
        let inherited = [
            "PATH",
            "VSCODE_IPC_HOOK_CLI",
            "PUDDLE_AGENT_LISTEN",
            "PORT",
            "HOME",
        ]
        .map(String::from);
        let password = puddle_types::fixture::password("editor");
        let launch = Launch {
            program: Path::new("/puddle/code-server").join("bin/code-server"),
            args: arguments(&server()),
            password_env: true,
            build: None,
        };
        let cmd = command(&launch, &password, inherited);
        let envs: std::collections::BTreeMap<String, Option<String>> = cmd
            .as_std()
            .get_envs()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.map(|v| v.to_string_lossy().into_owned()),
                )
            })
            .collect();
        assert_eq!(envs.get("PASSWORD"), Some(&Some(password)));
        for gone in ["VSCODE_IPC_HOOK_CLI", "PUDDLE_AGENT_LISTEN", "PORT"] {
            assert_eq!(envs.get(gone), Some(&None), "{gone} is removed");
        }
        for kept in ["PATH", "HOME"] {
            assert!(!envs.contains_key(kept), "{kept} is inherited untouched");
        }
        assert_eq!(
            cmd.as_std().get_program(),
            Path::new("/puddle/code-server")
                .join("bin/code-server")
                .as_os_str()
        );
    }

    #[test]
    fn a_switch_made_before_the_server_reads_its_build_is_not_a_restart_request() {
        let dir = tempfile::tempdir().unwrap();
        let builds = dir.path().join("servers");
        let (old, new) = ("1".repeat(40), "2".repeat(40));
        for commit in [&old, &new] {
            let program = builds.join(commit).join(microsoft::PROGRAM);
            std::fs::create_dir_all(program.parent().unwrap()).unwrap();
            std::fs::write(&program, "").unwrap();
            std::fs::write(builds.join(commit).join(".puddle-installed"), "").unwrap();
        }
        std::fs::write(builds.join("current"), &old).unwrap();
        let mut server = EditorServer::microsoft("/d", "/f", 120, false, true);
        server.dir = builds.to_string_lossy().into_owned();
        let control = Control::new(&server);
        assert_eq!(control.claim(), Some((builds.join(&old), false)));
        // Staged and switched while no server runs: the next read gets the new build, on trial,
        // and the request that switch made is spent by that read, not left to stop the server
        // that is about to start.
        std::fs::write(builds.join("staged"), &new).unwrap();
        assert_eq!(control.switch().unwrap(), Some(new.clone()));
        assert_eq!(control.claim(), Some((builds.join(&new), true)));
        assert!(control.restart.notified().now_or_never().is_none());
        // A switch after the read is a request the running server answers.
        std::fs::write(builds.join("staged"), &old).unwrap();
        assert_eq!(control.switch().unwrap(), Some(old));
        assert!(control.restart.notified().now_or_never().is_some());
    }

    #[test]
    fn restarts_wait_longer_after_each_quick_exit_up_to_a_limit() {
        let waits: Vec<u64> = [1, 2, 3, 10, 30, 31, 1000]
            .into_iter()
            .map(|n| wait_after(n).as_secs())
            .collect();
        assert_eq!(waits, [1, 2, 3, 10, 30, 30, 30]);
        assert_eq!(wait_after(0).as_secs(), 1);
    }

    #[test]
    fn a_refused_download_waits_a_minute_then_doubles_up_to_an_hour() {
        let waits: Vec<u64> = [0, 1, 2, 3, 6, 7, 8, 1000, u32::MAX]
            .into_iter()
            .map(|n| wait_after_refusal(n).as_secs())
            .collect();
        assert_eq!(waits, [60, 60, 120, 240, 1920, 3600, 3600, 3600, 3600]);
    }

    /// An update service that answers every metadata request with one build and a published hash
    /// that is not the tarball's, and counts the downloads.
    struct WrongHash {
        downloads: std::sync::atomic::AtomicUsize,
    }

    impl Fetch for Arc<WrongHash> {
        async fn text(&self, url: &str) -> io::Result<String> {
            tokio::task::yield_now().await; // a network answer is not instant
            let commit = "e".repeat(40);
            Ok(if url.contains("/api/latest/") {
                format!(r#"{{"version":"{commit}","productVersion":"1.0.0"}}"#)
            } else {
                format!(
                    r#"{{"version":"{commit}","sha256hash":"{}"}}"#,
                    "0".repeat(64)
                )
            })
        }

        async fn file(&self, _url: &str, dest: &Path) -> io::Result<()> {
            tokio::task::yield_now().await; // a download is not instant
            self.downloads
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            std::fs::write(dest, b"not the published file")
        }
    }

    /// A Microsoft server config on a fresh volume, and the service that refuses every build.
    fn refusing(dir: &Path, auto_update: bool) -> (EditorConfig, Arc<WrongHash>) {
        let mut server = EditorServer::microsoft("/d", "/f", 120, false, auto_update);
        server.dir = dir.join("servers").to_string_lossy().into_owned();
        server.data = dir.join("data").to_string_lossy().into_owned();
        let config = EditorConfig {
            server,
            password_file: dir.join("password"),
        };
        let service = Arc::new(WrongHash {
            downloads: std::sync::atomic::AtomicUsize::new(0),
        });
        (config, service)
    }

    #[tokio::test(start_paused = true)]
    async fn a_download_with_the_wrong_hash_is_fetched_on_a_slowing_schedule_not_every_30_seconds()
    {
        use std::sync::atomic::Ordering::SeqCst;
        let dir = tempfile::tempdir().unwrap();
        let (config, service) = refusing(dir.path(), false);
        let running = tokio::spawn(run_with(config, Arc::clone(&service)));
        // Downloads at 0, 1, 3, 7, 15, 31 and 63 minutes; the next is at 123. Every 30 seconds
        // would be 200.
        tokio::time::sleep(Duration::from_mins(100)).await;
        assert_eq!(service.downloads.load(SeqCst), 7);
        running.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn every_refused_download_is_told_to_the_host_in_words_without_checksums() {
        let dir = tempfile::tempdir().unwrap();
        let (config, service) = refusing(dir.path(), false);
        let control = Control::new(&config.server);
        let (out, mut host) = mpsc::channel(8);
        tokio::spawn(forward_install_problem(control.install_problem(), out));
        let running = tokio::spawn(run_controlled(config, service, Arc::clone(&control)));
        let wanted = AgentMessage::editor_status(Some(
            "the download does not match the checksum Microsoft published for it",
        ));
        // The first refusal, then the next one a minute later: a host that missed the first
        // (it restarted) hears the second.
        assert_eq!(host.recv().await, Some(wanted.clone()));
        assert_eq!(host.recv().await, Some(wanted));
        running.abort();
    }

    #[tokio::test]
    async fn an_install_that_works_ends_the_report_once_and_says_nothing_when_nothing_was_reported()
    {
        let control = Control::new(&EditorServer::microsoft("/d", "/f", 120, false, false));
        let (out, mut host) = mpsc::channel(8);
        tokio::spawn(forward_install_problem(control.install_problem(), out));
        control.install_worked();
        tokio::task::yield_now().await;
        assert!(
            host.try_recv().is_err(),
            "nothing was wrong, nothing to end"
        );
        control.install_refused("nope");
        assert_eq!(
            host.recv().await,
            Some(AgentMessage::editor_status(Some("nope")))
        );
        control.install_worked();
        assert_eq!(host.recv().await, Some(AgentMessage::editor_status(None)));
        control.install_worked();
        tokio::task::yield_now().await;
        assert!(host.try_recv().is_err(), "ended once");
    }

    #[tokio::test(start_paused = true)]
    async fn a_restart_request_ends_the_wait_after_a_refused_download() {
        use std::sync::atomic::Ordering::SeqCst;
        let dir = tempfile::tempdir().unwrap();
        let (config, service) = refusing(dir.path(), false);
        let control = Control::new(&config.server);
        let running = tokio::spawn(run_controlled(
            config,
            Arc::clone(&service),
            Arc::clone(&control),
        ));
        tokio::time::sleep(Duration::from_secs(10)).await;
        assert_eq!(service.downloads.load(SeqCst), 1);
        control.restart.notify_one();
        tokio::time::sleep(Duration::from_secs(10)).await;
        assert_eq!(service.downloads.load(SeqCst), 2, "tried again at once");
        running.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn a_refused_update_is_looked_for_on_a_slowing_schedule_too() {
        use std::sync::atomic::Ordering::SeqCst;
        let dir = tempfile::tempdir().unwrap();
        let (config, service) = refusing(dir.path(), true);
        let times = UpdateTimes {
            first: Duration::from_secs(1),
            every: Duration::from_hours(24),
            retry: Duration::from_mins(1),
        };
        let control = Control::with_times(&config.server, times);
        let looking = tokio::spawn(update_loop(control, Arc::new(Arc::clone(&service))));
        // Looks at 1 s and then after 1, 2, 4, 8, 16 and 32 minutes (7 in all); every minute would be 100.
        tokio::time::sleep(Duration::from_mins(100)).await;
        assert_eq!(service.downloads.load(SeqCst), 7);
        looking.abort();
    }
}
