// SPDX-License-Identifier: AGPL-3.0-or-later
//! A desktop VS Code client for VM tests: the pinned release, started on Remote-SSH and driven over
//! the DevTools Protocol, beside the VM.
//!
//! This is the editor a user attaches with (`Open in VS Code`): a real window with an Electron
//! renderer, not the browser editor. Nothing of the machine's own VS Code is touched: the window gets
//! a user-data folder and an extensions folder of its own, Puddle's ssh config, and no signed-in
//! account.
//!
//! - [`VsCodeRelease`] is the folder `ci/fetch-vscode.sh` makes (the pinned release unpacked, and the
//!   Remote-SSH package), named by `PUDDLE_VSCODE_DIR`.
//! - [`VsCodeLaunch`] says how to start it (folders, ports, ssh config, the extension under test) and
//!   [`VsCodeLaunch::start`] starts it and connects the driver (`vscode/driver.mjs`).
//! - [`VsCodeWindow`] is the running window: wait until it shows the remote's folders, run a command
//!   from the command palette, type a line in a new terminal, reload, take a picture.
//!
//! Playwright comes from the UI's lockfile, as for the browser client (`PUDDLE_VM_BROWSER_MODULES`).

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};

use crate::browser::{BROWSER_MODULES_VAR, Node};
use crate::driver::Driver;
use crate::error::HarnessError;

/// Environment variable naming the folder `ci/fetch-vscode.sh` made.
pub const VSCODE_DIR_VAR: &str = "PUDDLE_VSCODE_DIR";

/// The driver script, run by node.
const DRIVER: &str = include_str!("../vscode/driver.mjs");

/// How long node gets beyond a step's own budget.
const GRACE: Duration = Duration::from_secs(20);

/// How long a step that is not about the network may take (opening the palette, a terminal).
const STEP: Duration = Duration::from_secs(60);

/// How long the window may take to show a debug port and a workbench (a cold start of Electron on a
/// loaded runner takes tens of seconds).
const CONNECT: Duration = Duration::from_secs(120);

/// The pinned VS Code release and the Remote-SSH package that go with it.
#[derive(Debug, Clone)]
pub struct VsCodeRelease {
    dir: PathBuf,
}

impl VsCodeRelease {
    /// The release `PUDDLE_VSCODE_DIR` names.
    ///
    /// # Errors
    ///
    /// [`HarnessError::MissingVar`] when it is not set, [`HarnessError::VsCodeRelease`] when the
    /// folder is not a fetched release.
    pub fn from_env() -> Result<Self, HarnessError> {
        Self::from_var(std::env::var_os(VSCODE_DIR_VAR))
    }

    fn from_var(dir: Option<OsString>) -> Result<Self, HarnessError> {
        let dir = dir.ok_or(HarnessError::MissingVar {
            var: VSCODE_DIR_VAR,
            hint: "the folder ci/fetch-vscode.sh made",
        })?;
        Self::new(PathBuf::from(dir))
    }

    /// The release in `dir`.
    ///
    /// # Errors
    ///
    /// [`HarnessError::VsCodeRelease`] when it has no `app/Code.exe` or no `remote-ssh.vsix`.
    pub fn new(dir: PathBuf) -> Result<Self, HarnessError> {
        let release = Self { dir };
        if release.executable().is_file() && release.remote_ssh().is_file() {
            Ok(release)
        } else {
            Err(HarnessError::VsCodeRelease { dir: release.dir })
        }
    }

    /// The program that starts the window.
    #[must_use]
    pub fn executable(&self) -> PathBuf {
        self.dir.join("app").join("Code.exe")
    }

    /// The command line tool (`code.cmd`), which installs extensions.
    #[must_use]
    pub fn command_line(&self) -> PathBuf {
        self.dir.join("app").join("bin").join("code.cmd")
    }

    /// The Remote-SSH extension package.
    #[must_use]
    pub fn remote_ssh(&self) -> PathBuf {
        self.dir.join("remote-ssh.vsix")
    }
}

/// How to start a window; [`VsCodeLaunch::start`] does it.
#[derive(Debug, Clone)]
pub struct VsCodeLaunch {
    /// The window's user-data folder.
    pub user_data: PathBuf,
    /// The window's extensions folder (Remote-SSH is installed into it).
    pub extensions: PathBuf,
    /// Puddle's ssh config, the file the alias lives in.
    pub ssh_config: PathBuf,
    /// OpenSSH's `ssh`, which Remote-SSH runs.
    pub ssh: PathBuf,
    /// The host alias in the ssh config.
    pub alias: String,
    /// `remote.SSH.useLocalServer`; unset leaves VS Code's default.
    pub use_local_server: Option<bool>,
    /// The debug port the window opens.
    pub port: u16,
    /// What to open: the arguments the product's attach gave its `code` command
    /// (`--folder-uri` or `--file-uri`, then the address).
    pub open: Vec<String>,
    /// An extension folder loaded as an extension under development.
    pub dev_extension: Option<PathBuf>,
    /// A named VS Code profile to open the window in.
    pub profile: Option<String>,
    /// Environment variables for the window, which its extension hosts inherit.
    pub env: Vec<(String, String)>,
}

impl VsCodeLaunch {
    /// The `settings.json` of the window: Puddle's ssh config and the remote's platform, no
    /// telemetry, no update, no trust prompt, no recommendation, no automatic fetch.
    #[must_use]
    pub fn settings(&self) -> Value {
        let mut settings = json!({
            "remote.SSH.configFile": self.ssh_config.display().to_string(),
            "remote.SSH.path": self.ssh.display().to_string(),
            "remote.SSH.remotePlatform": { self.alias.clone(): "linux" },
            // The guest has no route to the download server; the host downloads and uploads.
            "remote.SSH.localServerDownload": "always",
            "remote.SSH.showLoginTerminal": false,
            "remote.SSH.connectTimeout": 120,
            "security.workspace.trust.enabled": false,
            "telemetry.telemetryLevel": "off",
            "update.mode": "none",
            "workbench.startupEditor": "none",
            // The first-run sign-in dialog for Copilot is a modal over the window and the account asks.
            "chat.disableAIFeatures": true,
            "extensions.autoUpdate": false,
            "extensions.autoCheckUpdates": false,
            "extensions.ignoreRecommendations": true,
            "git.autofetch": false,
            "terminal.integrated.defaultProfile.windows": "PowerShell",
            "window.restoreWindows": "none",
        });
        if let (Some(on), Some(map)) = (self.use_local_server, settings.as_object_mut()) {
            map.insert("remote.SSH.useLocalServer".to_owned(), json!(on));
        }
        settings
    }

    /// The command line of the window. The built-in GitHub sign-in is always off: no account of
    /// the machine's user may reach a test (a test that wants one stands in for it).
    #[must_use]
    pub fn arguments(&self) -> Vec<OsString> {
        let mut args: Vec<OsString> = vec![
            arg("--user-data-dir=", &self.user_data),
            arg("--extensions-dir=", &self.extensions),
            format!("--remote-debugging-port={}", self.port).into(),
            "--new-window".into(),
            "--disable-workspace-trust".into(),
            "--disable-updates".into(),
            "--skip-release-notes".into(),
            "--disable-extension=vscode.github-authentication".into(),
        ];
        if let Some(profile) = &self.profile {
            args.push(format!("--profile={profile}").into());
        }
        if let Some(dir) = &self.dev_extension {
            args.push(arg("--extensionDevelopmentPath=", dir));
        }
        args.extend(self.open.iter().map(OsString::from));
        args
    }

    /// Writes the settings and installs Remote-SSH into the extensions folder.
    ///
    /// # Errors
    ///
    /// [`HarnessError::Io`] when a file can't be written or the command line tool can't run,
    /// [`HarnessError::Browser`] when it refuses the package.
    pub async fn prepare(&self, release: &VsCodeRelease) -> Result<(), HarnessError> {
        let user = self.user_data.join("User");
        std::fs::create_dir_all(&user).map_err(|e| HarnessError::io("create", &user, e))?;
        let file = user.join("settings.json");
        std::fs::write(&file, self.settings().to_string())
            .map_err(|e| HarnessError::io("write", &file, e))?;
        self.install_remote_ssh(release, None).await
    }

    /// Gives the named profile what the default profile has: Remote-SSH, which a profile does not
    /// inherit, and the settings. The profile must exist, which a first start with it makes.
    ///
    /// # Errors
    ///
    /// As [`Self::prepare`], and [`HarnessError::Browser`] when no profile has been made yet.
    pub async fn prepare_profile(&self, release: &VsCodeRelease) -> Result<(), HarnessError> {
        let Some(profile) = &self.profile else {
            return Ok(());
        };
        self.install_remote_ssh(release, Some(profile)).await?;
        let profiles = self.user_data.join("User").join("profiles");
        let dirs = std::fs::read_dir(&profiles)
            .map_err(|e| HarnessError::io("read", &profiles, e))?
            .filter_map(Result::ok)
            .filter(|entry| entry.path().is_dir());
        for dir in dirs {
            let file = dir.path().join("settings.json");
            std::fs::write(&file, self.settings().to_string())
                .map_err(|e| HarnessError::io("write", &file, e))?;
        }
        Ok(())
    }

    async fn install_remote_ssh(
        &self,
        release: &VsCodeRelease,
        profile: Option<&str>,
    ) -> Result<(), HarnessError> {
        let cli = release.command_line();
        let mut command = tokio::process::Command::new(&cli);
        command
            .arg(arg("--user-data-dir=", &self.user_data))
            .arg(arg("--extensions-dir=", &self.extensions));
        if let Some(profile) = profile {
            command.arg(format!("--profile={profile}"));
        }
        let out = command
            .arg("--install-extension")
            .arg(release.remote_ssh())
            .stdin(Stdio::null())
            .output()
            .await
            .map_err(|e| HarnessError::io("run", &cli, e))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(HarnessError::Browser {
                detail: format!(
                    "installing Remote-SSH failed: {}{}",
                    String::from_utf8_lossy(&out.stdout),
                    String::from_utf8_lossy(&out.stderr)
                ),
            })
        }
    }

    /// Starts the window with `release`, sending its output to `log`, and connects the driver.
    ///
    /// # Errors
    ///
    /// [`HarnessError::Io`] when a program can't start, [`HarnessError::Browser`] when no window
    /// answers on the debug port in time.
    pub async fn start(
        &self,
        release: &VsCodeRelease,
        node: &Node,
        modules: &Path,
        log: &Path,
    ) -> Result<VsCodeWindow, HarnessError> {
        VsCodeWindow::start(
            &release.executable(),
            &self.arguments(),
            &Wiring {
                port: self.port,
                env: &self.env,
                node,
                modules,
                log,
                connect: CONNECT,
            },
        )
        .await
    }
}

fn arg(prefix: &str, path: &Path) -> OsString {
    let mut out = OsString::from(prefix);
    out.push(path);
    out
}

/// What the window showed once it was attached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attached {
    /// The explorer's top-level rows: one per folder of the workspace.
    pub folders: Vec<String>,
    /// The remote indicator, such as `SSH: puddle-demo`.
    pub remote: String,
    /// The window's title.
    pub title: String,
}

/// A running VS Code window with a driver connected to it. Dropping it stops the window.
#[derive(Debug)]
pub struct VsCodeWindow {
    child: tokio::process::Child,
    driver: Option<Driver>,
}

/// What a window is started with besides its program and arguments.
struct Wiring<'a> {
    port: u16,
    env: &'a [(String, String)],
    node: &'a Node,
    modules: &'a Path,
    log: &'a Path,
    connect: Duration,
}

impl VsCodeWindow {
    async fn start(
        program: &Path,
        args: &[OsString],
        wiring: &Wiring<'_>,
    ) -> Result<Self, HarnessError> {
        let Wiring {
            port,
            env,
            node,
            modules,
            log,
            connect,
        } = *wiring;
        let out = std::fs::File::create(log).map_err(|e| HarnessError::io("create", log, e))?;
        let err = out
            .try_clone()
            .map_err(|e| HarnessError::io("clone", log, e))?;
        let child = tokio::process::Command::new(program)
            .args(args)
            .envs(env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::null())
            .stdout(out)
            .stderr(err)
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| HarnessError::io("start", program, e))?;
        let driver = Driver::spawn(
            node,
            DRIVER,
            &[(BROWSER_MODULES_VAR, modules.as_os_str())],
            GRACE,
            "VS Code client",
            "closing the VS Code client",
        );
        // The window is stopped by `Drop` from here on, whatever fails.
        let mut window = Self {
            child,
            driver: None,
        };
        let mut driver = driver?;
        driver
            .call(
                &json!({"op": "connect", "port": port, "timeoutMs": millis(connect)}),
                connect,
            )
            .await?;
        window.driver = Some(driver);
        Ok(window)
    }

    async fn call(&mut self, command: Value, budget: Duration) -> Result<Value, HarnessError> {
        let driver = self.driver.as_mut().ok_or_else(|| HarnessError::Browser {
            detail: "the window is closed".to_owned(),
        })?;
        driver.call(&command, budget).await.map(|o| o.result)
    }

    /// Waits until the workbench shows a folder in the explorer and the remote indicator says
    /// `SSH:`, then reports what it shows.
    ///
    /// # Errors
    ///
    /// [`HarnessError::Browser`] when it did not happen within `within`.
    pub async fn attached(&mut self, within: Duration) -> Result<Attached, HarnessError> {
        let seen = self
            .call(
                json!({"op": "attached", "timeoutMs": millis(within)}),
                within,
            )
            .await?;
        Ok(Self::state(&seen))
    }

    fn state(seen: &Value) -> Attached {
        let text = |key: &str| {
            seen.get(key)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        Attached {
            folders: seen
                .get("folders")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|f| f.as_str().map(str::to_owned))
                .collect(),
            remote: text("remote"),
            title: text("title"),
        }
    }

    /// Runs a command from the command palette, by its exact label.
    ///
    /// # Errors
    ///
    /// [`HarnessError::Browser`] when the palette has no row with exactly that label.
    pub async fn palette(&mut self, label: &str) -> Result<(), HarnessError> {
        self.call(
            json!({"op": "palette", "label": label, "timeoutMs": millis(STEP)}),
            STEP,
        )
        .await
        .map(drop)
    }

    /// Opens a new terminal in the window and types `line` and Enter into it.
    ///
    /// # Errors
    ///
    /// [`HarnessError::Browser`] when no terminal opened.
    pub async fn terminal(&mut self, line: &str) -> Result<(), HarnessError> {
        self.call(
            json!({"op": "terminal", "line": line, "timeoutMs": millis(STEP)}),
            STEP,
        )
        .await
        .map(drop)
    }

    /// Answers the input box the window shows the person (an extension asking for a value), if one
    /// shows within `within`, with Escape, and returns its text; `None` when the window asks nothing.
    ///
    /// # Errors
    ///
    /// [`HarnessError::Browser`] when the input box stays open after Escape.
    pub async fn dismiss_prompt(
        &mut self,
        within: Duration,
    ) -> Result<Option<String>, HarnessError> {
        let seen = self
            .call(
                json!({"op": "prompt", "timeoutMs": millis(within)}),
                within * 2 + STEP,
            )
            .await?;
        Ok(seen.as_str().map(str::to_owned))
    }

    /// Runs Developer: Reload Window and waits until the window is attached again.
    ///
    /// # Errors
    ///
    /// [`HarnessError::Browser`] when the window did not come back within `within`; it says what the
    /// window showed, and a picture of it goes to `picture`.
    pub async fn reload(
        &mut self,
        within: Duration,
        picture: &Path,
    ) -> Result<Attached, HarnessError> {
        let seen = self
            .call(
                json!({"op": "reload", "timeoutMs": millis(within), "picture": picture.display().to_string()}),
                within * 2 + STEP,
            )
            .await?;
        Ok(Self::state(&seen))
    }

    /// Evaluates an expression in the window and returns its value.
    ///
    /// # Errors
    ///
    /// [`HarnessError::Browser`] when the expression throws.
    pub async fn evaluate(&mut self, expression: &str) -> Result<Value, HarnessError> {
        self.call(json!({"op": "eval", "expression": expression}), STEP)
            .await
    }

    /// Saves a picture of the window.
    ///
    /// # Errors
    ///
    /// [`HarnessError::Browser`] when the window can't be captured.
    pub async fn screenshot(&mut self, path: &Path) -> Result<(), HarnessError> {
        self.call(
            json!({"op": "screenshot", "path": path.display().to_string()}),
            STEP,
        )
        .await
        .map(drop)
    }

    /// Stops the driver and the window with all its processes.
    ///
    /// # Errors
    ///
    /// [`HarnessError::Timeout`] when the driver did not end in time (the window is stopped
    /// anyway).
    pub async fn close(mut self) -> Result<(), HarnessError> {
        let ended = match self.driver.take() {
            Some(driver) => driver.quit().await,
            None => Ok(()),
        };
        stop_tree(&self.child);
        ended
    }
}

impl Drop for VsCodeWindow {
    fn drop(&mut self) {
        stop_tree(&self.child);
    }
}

/// Stops the window and everything it started: Electron runs a process per window and utility, and
/// ending the first leaves the rest running on Windows.
#[cfg(windows)]
fn stop_tree(child: &tokio::process::Child) {
    let Some(pid) = child.id() else { return };
    let _ = std::process::Command::new("taskkill")
        .args(["/T", "/F", "/PID"])
        .arg(pid.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Elsewhere the process group ends with its first process (and the child is killed on drop).
#[cfg(not(windows))]
fn stop_tree(_child: &tokio::process::Child) {}

/// The code a hostile guest appends to its git extension's `dist/main.js` (U7c): it leaves a marker
/// file in the guest, proof that the patched file ran, then does what the guest can do through its
/// VS Code server: open a terminal in the window's own host and type a command into it.
/// The command writes the Windows user's name to `probe`, a Windows path.
#[must_use]
pub fn local_terminal_probe(marker: &str, probe: &str) -> String {
    let probe = probe.replace('\\', "\\\\");
    format!(
        "(async () => {{ try {{ require('fs').writeFileSync('{marker}', String(Date.now())); const v = require('vscode'); \
         await v.commands.executeCommand('workbench.action.terminal.newLocal'); \
         await v.commands.executeCommand('workbench.action.terminal.sendSequence', {{ text: 'whoami > {probe}\\r' }}); \
         }} catch (e) {{ require('fs').writeFileSync('{marker}.err', String(e)); }} }})();"
    )
}

fn millis(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests;
