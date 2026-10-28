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
use puddle_agent_proto::editor::{EditorServer, ServerKind};
use tokio::process::Command;
use tokio::sync::{Mutex, Notify};

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
/// The longest wait between two starts.
const MAX_WAIT: Duration = Duration::from_secs(30);

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
    times: UpdateTimes,
    /// Whether this server updates at all (Microsoft's, with the user's setting on).
    updates: bool,
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
            times,
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
        let switched = microsoft::switch_to_staged(&self.builds)?;
        if switched.is_some() {
            self.restart.notify_one();
        }
        Ok(switched)
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

/// What to start: the program, its arguments, and the password for its environment (code-server
/// reads it there; Microsoft's server reads its token from a file named in the arguments).
struct Launch {
    program: PathBuf,
    args: Vec<OsString>,
    password_env: bool,
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
) -> Result<Launch, String> {
    let server = &config.server;
    match server.kind {
        ServerKind::CodeServer => Ok(Launch {
            program: Path::new(&server.dir).join("bin/code-server"),
            args: arguments(server),
            password_env: true,
        }),
        ServerKind::Microsoft { telemetry, .. } => {
            microsoft::prepare(server).map_err(|e| e.to_string())?;
            let platform =
                microsoft::platform(std::env::consts::ARCH).map_err(|e| e.to_string())?;
            let build = if let Some(build) = microsoft::current_build(&control.builds) {
                build
            } else {
                // Not while an update download runs: it may be fetching this very build.
                let _work = control.work.lock().await;
                microsoft::ensure_build(fetch, platform, &control.builds)
                    .await
                    .map_err(|e| e.to_string())?
            };
            Ok(Launch {
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

/// Looks for a newer build now and then, and stages what it finds. Never returns.
async fn update_loop(control: Arc<Control>, fetch: Arc<impl Fetch>) {
    let Ok(platform) = microsoft::platform(std::env::consts::ARCH) else {
        return;
    };
    tokio::time::sleep(control.times.first).await;
    loop {
        let looked = {
            let _work = control.work.lock().await;
            microsoft::stage_update(&*fetch, platform, &control.builds).await
        };
        let wait = match looked {
            Ok(microsoft::Update::UpToDate) => {
                tracing::debug!("the editor server is the newest build");
                control.times.every
            }
            Ok(microsoft::Update::Staged(commit)) => {
                tracing::info!(%commit, "a newer editor server build is ready for the next quiet moment");
                control.times.every
            }
            Err(err) => {
                tracing::warn!(%err, "could not look for a newer editor server build");
                control.times.retry
            }
        };
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
    loop {
        let launched = launch(config, fetch, control).await;
        let started = Instant::now();
        match launched {
            Ok(launch) => match start_once(config, &launch, control).await {
                Ok(status) => tracing::warn!(%status, "the editor server exited"),
                Err(err) => tracing::error!(error = %err, "the editor server could not run"),
            },
            // Counted as a quick exit: a missing network must not be retried every second.
            Err(reason) => {
                tracing::error!(%reason, "the editor server could not be made ready");
                quick = quick.saturating_add(1);
                tokio::time::sleep(wait_after(quick)).await;
                continue;
            }
        }
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
async fn confirm_when_listening(control: Arc<Control>, port: u16) {
    let deadline = Instant::now() + STARTUP;
    while Instant::now() < deadline {
        if port_taken(port).await {
            let _work = control.work.lock().await;
            if let Err(err) = microsoft::confirm(&control.builds) {
                tracing::warn!(%err, "could not record that the editor server build started");
            }
            return;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

/// After a server exited: a build that exited before it ever accepted a connection is dropped
/// for the build it replaced.
async fn settle_trial(control: &Control) {
    let _work = control.work.lock().await;
    match microsoft::roll_back(&control.builds) {
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
    tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .is_ok()
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
    // A restart asked for while no server ran is answered by this start already (it reads the
    // current build), so a request left over is not one for the server about to run.
    let _ = control.restart.notified().now_or_never();
    let mut child = command(launch, &password, inherited).spawn()?;
    tracing::info!(
        port = config.server.port,
        grace_secs = config.server.grace_secs,
        "the editor server started"
    );
    let microsoft = matches!(config.server.kind, ServerKind::Microsoft { .. });
    // Whether this server runs a build that has not run before and has one to fall back to.
    let on_trial = microsoft && microsoft::on_trial(&control.builds);
    let confirming = microsoft.then(|| {
        tokio::spawn(confirm_when_listening(
            Arc::clone(control),
            config.server.port,
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
    if on_trial && !replaced {
        settle_trial(control).await;
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
        asked: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        free: std::sync::Arc<std::sync::atomic::AtomicBool>,
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
    fn restarts_wait_longer_after_each_quick_exit_up_to_a_limit() {
        let waits: Vec<u64> = [1, 2, 3, 10, 30, 31, 1000]
            .into_iter()
            .map(|n| wait_after(n).as_secs())
            .collect();
        assert_eq!(waits, [1, 2, 3, 10, 30, 30, 30]);
        assert_eq!(wait_after(0).as_secs(), 1);
    }
}
