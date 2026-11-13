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
//!   holds one of this boot, so a window signed in stays signed in; a rebooted sandbox gets a new
//!   one (the guest's `/run` is on its disk, so a file beside the password names its boot);
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
use puddle_agent_proto::editor::{EditorServer, NEEDS_GLIBC, ServerKind};
use puddle_agent_proto::{AgentMessage, EditorProblemKind};
use tokio::process::Command;
use tokio::sync::{Mutex, Notify, mpsc, watch};

use crate::account::Account;
use crate::download::{Downloader, Fetch};
use crate::microsoft;

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

/// How many tries in a row that fail on something other than a refusal (the network is down)
/// pass before the user is told.
const TRIES_BEFORE_REPORT: u32 = 5;

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
    /// Why the server cannot be installed, `None` when it can: every refused or failed download
    /// sets it again (also to the same text), so a host that missed one hears the next.
    install_problem: watch::Sender<Option<String>>,
    /// Why a newer build could not be taken, `None` when nothing is wrong; set again in the same
    /// way. The server runs on the build it has meanwhile.
    update_problem: watch::Sender<Option<String>>,
    /// Why the server runs as root though a session user was named, `None` when it runs as the
    /// user it should; set again at each start that lacks the user.
    session_user_problem: watch::Sender<Option<String>>,
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
            update_problem: watch::channel(None).0,
            session_user_problem: watch::channel(None).0,
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
    /// Follows why the server cannot be installed ([`forward_editor_problems`] sends it to the
    /// host). Subscribe before the server runs: a receiver sees only what is set after it exists.
    #[must_use]
    pub fn install_problem(&self) -> watch::Receiver<Option<String>> {
        self.install_problem.subscribe()
    }

    /// Follows why a newer build could not be taken, like [`Control::install_problem`].
    #[must_use]
    pub fn update_problem(&self) -> watch::Receiver<Option<String>> {
        self.update_problem.subscribe()
    }

    /// Follows why the server runs as root though a session user was named, like
    /// [`Control::install_problem`].
    #[must_use]
    pub fn session_user_problem(&self) -> watch::Receiver<Option<String>> {
        self.session_user_problem.subscribe()
    }

    /// A start found no session user in the guest: say why, every time.
    fn session_user_missing(&self, reason: &str) {
        self.session_user_problem
            .send_replace(Some(reason.to_owned()));
    }

    /// A start found the session user, or none was named: ends what
    /// [`Control::session_user_missing`] said, once.
    fn session_user_found(&self) {
        end(&self.session_user_problem);
    }

    /// A download was refused, or failed on the network again and again: say why, every time.
    fn install_failed(&self, reason: &str) {
        self.install_problem.send_replace(Some(reason.to_owned()));
    }

    /// The server got past its install: ends what [`Control::install_failed`] said, once.
    fn install_worked(&self) {
        end(&self.install_problem);
    }

    /// A newer build was refused, started badly or could not be looked for: say why, every time.
    fn update_failed(&self, reason: &str) {
        self.update_problem.send_replace(Some(reason.to_owned()));
    }

    /// A look for a newer build worked: ends what [`Control::update_failed`] said, once.
    fn update_worked(&self) {
        end(&self.update_problem);
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
    /// The kernel's id of this boot ([`BOOT_ID_FILE`]). A password is kept only in the boot it was
    /// made in: the guest's `/run` is on its disk, so a file there outlives a stop and start.
    pub boot_id: PathBuf,
    /// The guest's root folder, where its loaders are looked for ([`unsupported_image`]).
    pub root: PathBuf,
}

/// Where the kernel keeps the id of the running boot.
pub const BOOT_ID_FILE: &str = "/proc/sys/kernel/random/boot_id";

/// The boot id in `path`, or `None` when it cannot be read (then no password is kept).
fn boot_id(path: &Path) -> Option<String> {
    let id = std::fs::read_to_string(path).ok()?;
    let id = id.trim();
    (!id.is_empty() && id.len() <= 64).then(|| id.to_owned())
}

/// The file beside the password that names the boot it was made in.
fn boot_stamp(password_file: &Path) -> PathBuf {
    let mut stamp = password_file.as_os_str().to_owned();
    stamp.push(".boot");
    PathBuf::from(stamp)
}

/// The password of this boot: the one in `config.password_file` when it was made in the boot
/// `config.boot_id` names, else a new one, written there.
fn boot_password(config: &EditorConfig) -> io::Result<String> {
    let boot = boot_id(&config.boot_id);
    let stamp = boot_stamp(&config.password_file);
    let made_in_this_boot = boot.is_some() && std::fs::read_to_string(&stamp).ok() == boot;
    if made_in_this_boot && let Some(password) = existing_password(&config.password_file) {
        return Ok(password);
    }
    let password = new_password()?;
    write_password(&config.password_file, &password)?;
    // An unknown boot leaves an empty stamp, which no boot matches.
    write_private(&stamp, boot.as_deref().unwrap_or_default())?;
    Ok(password)
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
    write_private(path, password)
}

/// Writes `text` to `path` for its owner only, through a temporary file renamed over it.
fn write_private(path: &Path, text: &str) -> io::Result<()> {
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
        file.write_all(text.as_bytes())?;
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
        // code-server sends a window opened without a folder to the one its last window showed,
        // over the target below; Puddle's target (the start folder or the multi-root file) wins.
        "--ignore-last-opened".into(),
        // The sandbox is the boundary, so the folder it opens is trusted: no "Trust Folder"
        // prompt, and the terminal's shell starts at once. Only this server gets the flag; a
        // desktop editor attached over SSH keeps its own prompt.
        "--disable-workspace-trust".into(),
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

/// The server's command: `launch`, the password in its environment when it takes one, none of
/// the variables in `inherited` that [`withheld`] names, and run as `account` with its login
/// variables when there is one (else as the agent's own user, root).
fn command(
    launch: &Launch,
    password: &str,
    inherited: impl IntoIterator<Item = String>,
    account: Option<&Account>,
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
    if let Some(account) = account {
        command.envs(account.env());
        #[cfg(target_os = "linux")]
        account.run_as(&mut command);
    }
    command
}

/// The account the server runs as, looked up with `read`: `None` for root, the agent's own user.
/// A session user the guest does not list (the boot hook adds it, so it was removed or cannot be read afterwards) leaves the server
/// running as root, so the editor still opens; the agent's log and the host (through `control`)
/// say why.
fn account(
    server: &EditorServer,
    control: &Control,
    read: impl FnOnce(&str) -> io::Result<Account>,
) -> Option<Account> {
    if server.user.is_root() {
        control.session_user_found();
        return None;
    }
    match read(server.user.name()) {
        Ok(account) => {
            control.session_user_found();
            Some(account)
        }
        Err(err) => {
            tracing::error!(user = %server.user, error = %err, "the editor server runs as root: its user is missing");
            control.session_user_missing(&format!(
                "the session user {} is missing in the guest ({err}), so the editor server runs as root",
                server.user
            ));
            None
        }
    }
}

/// Gives `account` the folders the agent made for the server and the password file (Microsoft's
/// server reads its token from it), so the server can write its data and read its token.
fn give_to(account: &Account, config: &EditorConfig) -> io::Result<()> {
    let data = Path::new(&config.server.data);
    let own_data = match config.server.kind {
        ServerKind::CodeServer => USER_DATA,
        ServerKind::Microsoft { .. } => microsoft::SERVER_DATA,
    };
    for path in [data, &data.join(own_data), &data.join(EXTENSIONS)] {
        account.own(path)?;
    }
    account.own(&config.password_file)
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

/// Ends a problem that is set, once; one that is not is left alone, so nothing is sent.
fn end(problem: &watch::Sender<Option<String>>) {
    problem.send_if_modified(|problem| problem.take().is_some());
}

/// Sends every change of the problems (a [`Control::install_problem`], a
/// [`Control::update_problem`] and a [`Control::session_user_problem`]) to the host as an [`AgentMessage::EditorStatus`]. Returns when
/// `out` closes.
pub async fn forward_editor_problems(
    mut install: watch::Receiver<Option<String>>,
    mut update: watch::Receiver<Option<String>>,
    mut session_user: watch::Receiver<Option<String>>,
    out: mpsc::Sender<AgentMessage>,
) {
    loop {
        let message = tokio::select! {
            changed = install.changed() => {
                if changed.is_err() { return; }
                AgentMessage::editor_status(
                    EditorProblemKind::Install,
                    install.borrow_and_update().as_deref(),
                )
            }
            changed = update.changed() => {
                if changed.is_err() { return; }
                AgentMessage::editor_status(
                    EditorProblemKind::Update,
                    update.borrow_and_update().as_deref(),
                )
            }
            changed = session_user.changed() => {
                if changed.is_err() { return; }
                AgentMessage::editor_status(
                    EditorProblemKind::SessionUser,
                    session_user.borrow_and_update().as_deref(),
                )
            }
        };
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
    let mut failed = 0_u32;
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
                failed = 0;
                tracing::warn!(%err, refused, "a newer editor server build was refused");
                control.update_failed(&format!(
                    "a newer build was refused: {}",
                    err.refusal_summary()
                ));
                wait_after_refusal(refused).max(control.times.retry)
            }
            Err(err) => {
                failed = failed.saturating_add(1);
                tracing::warn!(%err, failed, "could not look for a newer editor server build");
                // A look that fails now and then (the network was down a while) is not news.
                if failed >= TRIES_BEFORE_REPORT {
                    control
                        .update_failed(&format!("looking for a newer build keeps failing: {err}"));
                }
                control.times.retry
            }
        };
        if is_ok {
            refused = 0;
            failed = 0;
            control.update_worked();
        }
        tokio::time::sleep(wait).await;
    }
}

/// The loader directories of a guest, relative to its root.
const LOADER_DIRS: [&str; 4] = ["lib", "lib64", "usr/lib", "usr/lib64"];

/// Whether the guest under `root` has musl's loader and no glibc loader: neither editor server
/// starts there, because the node they bundle is linked against glibc. Alpine with `gcompat` has
/// a glibc loader name and counts as glibc.
#[must_use]
pub fn musl_without_glibc(root: &Path) -> bool {
    let (mut musl, mut glibc) = (false, false);
    for dir in LOADER_DIRS {
        let Ok(entries) = std::fs::read_dir(root.join(dir)) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            musl |= name.starts_with("ld-musl-");
            glibc |= name.starts_with("ld-linux-");
        }
    }
    musl && !glibc
}

/// What the host is told of an image the editor cannot run in, `None` for any other.
#[must_use]
pub fn unsupported_image(root: &Path) -> Option<&'static str> {
    musl_without_glibc(root).then_some(NEEDS_GLIBC)
}

async fn serve_forever(config: &EditorConfig, fetch: &impl Fetch, control: &Arc<Control>) {
    let server = &config.server;
    if let Some(reason) = unsupported_image(&config.root) {
        tracing::warn!(reason, "no editor server can run in this image");
        control.install_failed(reason);
        return;
    }
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
    let mut unready = 0_u32;
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
                unready = 0;
                let wait = wait_after_refusal(refused);
                tracing::error!(%reason, ?wait, refused, "the editor server download was refused; trying again later");
                control.install_failed(&summary);
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
                unready = unready.saturating_add(1);
                // A network that is down for a few seconds clears itself; one that stays down is
                // what the user must hear of.
                if unready >= TRIES_BEFORE_REPORT {
                    control.install_failed(&format!("the download keeps failing: {reason}"));
                }
                quick = quick.saturating_add(1);
                tokio::time::sleep(wait_after(quick)).await;
                continue;
            }
        }
        refused = 0;
        unready = 0;
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
        Ok(true) => {
            tracing::error!(
                "the new editor server build exited before it started; running the previous one again"
            );
            control.update_failed(
                "the newer build exited before it started, so the previous one runs again",
            );
        }
        Ok(false) => {}
        Err(err) => {
            tracing::warn!(%err, "could not go back to the previous editor server build");
            control.update_failed(&format!(
                "the newer build exited before it started and the previous one could not be restored: {err}"
            ));
        }
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
    let password = boot_password(config)?;
    let account = account(&config.server, control, Account::read);
    if let Some(account) = &account {
        give_to(account, config)?;
    }
    let inherited = std::env::vars_os().filter_map(|(k, _)| k.into_string().ok());
    let mut child = command(launch, &password, inherited, account.as_ref()).spawn()?;
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
mod tests;
