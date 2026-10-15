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
//! The server runs without the `VSCODE_*` variables of the agent's environment: with
//! `VSCODE_IPC_HOOK_CLI` set, code-server hands its command line to the editor that variable
//! names instead of serving.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use puddle_agent_proto::editor::EditorServer;
use tokio::process::Command;

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

/// The command line of the server (without the program): the release's own flags, the folder last.
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
    args.push(OsString::from(&server.folder));
    args
}

/// Whether the server must not inherit the variable `name`: editor hand-over variables, our own
/// settings, and `PORT` (which code-server reads in place of its bind address).
fn withheld(name: &str) -> bool {
    name.starts_with("VSCODE_") || name.starts_with("PUDDLE_AGENT_") || name == "PORT"
}

/// The server's command: the release's `bin/code-server`, the password in its environment, and
/// none of the variables in `inherited` that [`withheld`] names.
fn command(
    server: &EditorServer,
    password: &str,
    inherited: impl IntoIterator<Item = String>,
) -> Command {
    let mut command = Command::new(Path::new(&server.dir).join("bin/code-server"));
    command
        .args(arguments(server))
        .env("PASSWORD", password)
        .stdin(Stdio::null())
        .kill_on_drop(true);
    for name in inherited.into_iter().filter(|n| withheld(n)) {
        command.env_remove(name);
    }
    command
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
pub async fn run(config: EditorConfig) {
    let server = &config.server;
    let program = Path::new(&server.dir).join("bin/code-server");
    if !program.is_file() {
        // A sandbox made before the host bundled the server has no such mount; it gets one when
        // it is made again.
        tracing::warn!(program = %program.display(), "no editor server in this sandbox: it was made without the code-server mount");
        return;
    }
    let mut quick = 0_u32;
    loop {
        let started = Instant::now();
        match start_once(&config).await {
            Ok(status) => tracing::warn!(%status, "the editor server exited"),
            Err(err) => tracing::error!(error = %err, "the editor server could not run"),
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

/// Whether something already accepts connections on the server's port.
async fn port_taken(port: u16) -> bool {
    tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .is_ok()
}

async fn start_once(config: &EditorConfig) -> io::Result<std::process::ExitStatus> {
    prepare(&config.server)?;
    let mut warned = false;
    while port_taken(config.server.port).await {
        if !warned {
            tracing::warn!(
                port = config.server.port,
                "the editor port is taken (by a server an earlier agent started?); waiting for it to be free"
            );
            warned = true;
        }
        tokio::time::sleep(PORT_POLL).await;
    }
    let password = if let Some(password) = existing_password(&config.password_file) {
        password
    } else {
        let password = new_password()?;
        write_password(&config.password_file, &password)?;
        password
    };
    let inherited = std::env::vars_os().filter_map(|(k, _)| k.into_string().ok());
    let mut child = command(&config.server, &password, inherited).spawn()?;
    tracing::info!(
        port = config.server.port,
        grace_secs = config.server.grace_secs,
        "the editor server started"
    );
    child.wait().await
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
        for bad in [
            "",
            "short",
            &"g".repeat(64),
            &format!("{good}\n"),
            &format!("{good}00"),
        ] {
            std::fs::write(&path, bad).unwrap();
            assert_eq!(existing_password(&path), None, "{bad:?}");
        }
        std::fs::write(&path, &good).unwrap();
        assert_eq!(existing_password(&path), Some(good));
    }

    #[test]
    fn the_password_file_is_owner_only_and_replaced_whole() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("run/editor-password");
        write_password(&path, "first").unwrap();
        write_password(&path, "second").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "second");
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
        let cmd = command(&server(), "s3cret", inherited);
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
        assert_eq!(envs.get("PASSWORD"), Some(&Some("s3cret".to_owned())));
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
