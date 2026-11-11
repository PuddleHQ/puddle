// SPDX-License-Identifier: GPL-3.0-or-later
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
mod tests {
    use super::*;

    fn launch() -> VsCodeLaunch {
        VsCodeLaunch {
            user_data: PathBuf::from("/w/user"),
            extensions: PathBuf::from("/w/ext"),
            ssh_config: PathBuf::from("/w/ssh/config"),
            ssh: PathBuf::from("/bin/ssh"),
            alias: "puddle-demo".to_owned(),
            use_local_server: None,
            port: 9230,
            open: vec!["--file-uri".to_owned(), "vscode-remote://x/y".to_owned()],
            dev_extension: None,
            profile: None,
            env: Vec::new(),
        }
    }

    #[test]
    fn the_settings_name_the_ssh_config_the_alias_platform_and_keep_the_machine_out() {
        let settings = launch().settings();
        assert_eq!(settings["remote.SSH.configFile"], "/w/ssh/config");
        assert_eq!(settings["remote.SSH.path"], "/bin/ssh");
        assert_eq!(
            settings["remote.SSH.remotePlatform"],
            json!({"puddle-demo": "linux"})
        );
        assert_eq!(settings["telemetry.telemetryLevel"], "off");
        assert_eq!(settings["update.mode"], "none");
        assert_eq!(settings["security.workspace.trust.enabled"], false);
        assert!(settings.get("remote.SSH.useLocalServer").is_none());
        let local = VsCodeLaunch {
            use_local_server: Some(true),
            ..launch()
        };
        assert_eq!(local.settings()["remote.SSH.useLocalServer"], true);
        let remote = VsCodeLaunch {
            use_local_server: Some(false),
            ..launch()
        };
        assert_eq!(remote.settings()["remote.SSH.useLocalServer"], false);
    }

    #[test]
    fn the_command_line_isolates_the_window_opens_the_port_and_turns_the_github_sign_in_off() {
        let args: Vec<String> = launch()
            .arguments()
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args[0], "--user-data-dir=/w/user");
        assert_eq!(args[1], "--extensions-dir=/w/ext");
        assert!(args.contains(&"--remote-debugging-port=9230".to_owned()));
        assert!(args.contains(&"--disable-extension=vscode.github-authentication".to_owned()));
        assert!(!args.iter().any(|a| a.starts_with("--profile")));
        assert!(
            !args
                .iter()
                .any(|a| a.starts_with("--extensionDevelopmentPath"))
        );
        assert_eq!(
            args[args.len() - 2..],
            ["--file-uri", "vscode-remote://x/y"]
        );
        let more = VsCodeLaunch {
            dev_extension: Some(PathBuf::from("/e")),
            profile: Some("puddle-test".to_owned()),
            ..launch()
        };
        let args: Vec<String> = more
            .arguments()
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(args.contains(&"--profile=puddle-test".to_owned()));
        assert!(args.contains(&"--extensionDevelopmentPath=/e".to_owned()));
        // The address stays last: it is what the product's attach asked to open.
        assert_eq!(
            args[args.len() - 2..],
            ["--file-uri", "vscode-remote://x/y"]
        );
    }

    #[test]
    fn a_release_needs_the_executable_and_the_remote_ssh_package() {
        let dir = tempfile::tempdir().expect("tempdir");
        let err = VsCodeRelease::new(dir.path().to_owned()).expect_err("empty");
        assert!(err.to_string().contains(&dir.path().display().to_string()));
        std::fs::create_dir_all(dir.path().join("app").join("bin")).expect("mkdir");
        std::fs::write(dir.path().join("app").join("Code.exe"), "").expect("exe");
        VsCodeRelease::new(dir.path().to_owned()).expect_err("no package yet");
        std::fs::write(dir.path().join("remote-ssh.vsix"), "").expect("vsix");
        let release = VsCodeRelease::new(dir.path().to_owned()).expect("complete");
        assert!(release.executable().ends_with("Code.exe"));
        assert!(release.command_line().ends_with("code.cmd"));
        assert!(release.remote_ssh().ends_with("remote-ssh.vsix"));
    }

    #[test]
    fn the_release_needs_its_variable() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("app")).expect("mkdir");
        std::fs::write(dir.path().join("app").join("Code.exe"), "").expect("exe");
        std::fs::write(dir.path().join("remote-ssh.vsix"), "").expect("vsix");
        let release = VsCodeRelease::from_var(Some(dir.path().as_os_str().to_owned()))
            .expect("a complete folder");
        assert!(release.remote_ssh().starts_with(dir.path()));
        let err = VsCodeRelease::from_var(None).expect_err("unset");
        assert!(err.to_string().contains(VSCODE_DIR_VAR), "{err}");
        // The process's own environment: either answer is right, the line must run.
        let _ = VsCodeRelease::from_env();
    }

    /// A `playwright-core` that drives no VS Code. Its first two `connectOverCDP` calls are refused
    /// (the debug port is not open yet) and the third gives a window with one workbench page; a
    /// port of 1 is never answered. The page records what it was asked (`evaluate("calls")`) and
    /// answers `attached` with two folders; the first terminal asks for a folder, the second does not.
    fn stub_modules() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let core = dir.path().join("node_modules").join("playwright-core");
        std::fs::create_dir_all(&core).expect("mkdir");
        std::fs::write(core.join("package.json"), "{\"main\":\"index.js\"}").expect("package");
        std::fs::write(
            core.join("index.js"),
            "let tries = 0; let terminals = 0; const calls = [];
             let prompts = 0;
             const chain = (name) => ({
               waitFor: async (o) => {
                 calls.push(name + ' waitFor ' + o.state);
                 if (name === '.quick-input-widget:visible first' && o.state === 'visible' && ++prompts % 2 === 0) throw new Error('Timeout');
               },
               innerText: async () => 'Git: https://github.com',
               filter: (f) => chain(name + ' filter ' + f.hasText),
               first: () => chain(name + ' first'),
               click: async () => { calls.push(name + ' click'); },
               focus: async () => { calls.push(name + ' focus'); },
               isVisible: async () => name.includes('working directory') && ++terminals % 2 === 1,
             });
             const page = {
               url: () => 'vscode-file://vscode-app/workbench.html',
               on: () => {},
               keyboard: {
                 press: async (k) => { calls.push('press ' + k); },
                 insertText: async (t) => { calls.push('insert ' + t); },
                 type: async (t) => { calls.push('type ' + t); },
               },
               locator: (sel) => chain(sel),
               waitForFunction: async () => { calls.push('new document'); },
               evaluate: async (f) => {
                 if (f.toString().includes('__puddleOldDocument')) { calls.push('mark old document'); return; }
                 if (f.name === 'readState') return { folders: ['api', 'web'], remote: 'SSH: x', title: 't' };
                 const text = f.toString();
                 if (text.includes('(calls)')) return calls;
                 if (text.includes('boom')) throw new Error('boom');
                 return /return \\((.*)\\);/.exec(text)[1];
               },
               screenshot: async (o) => { calls.push('shot ' + o.path); },
             };
             exports.chromium = { connectOverCDP: async (url) => {
               if (url.endsWith(':1') || ++tries < 3) throw new Error('connect ECONNREFUSED');
               return { contexts: () => [{ pages: () => [page] }], close: async () => {} };
             } };",
        )
        .expect("index");
        dir
    }

    async fn stub_window(
        modules: &tempfile::TempDir,
        port: u16,
        connect: Duration,
    ) -> Result<VsCodeWindow, HarnessError> {
        let log = modules.path().join("code.log");
        let env = [(
            "PUDDLE_STUB".to_owned(),
            modules.path().join("env-seen").display().to_string(),
        )];
        let node = Node::from_env();
        let modules = modules.path().join("node_modules");
        VsCodeWindow::start(
            Path::new(&node.program),
            &[
                OsString::from("-e"),
                OsString::from(
                    "require('fs').writeFileSync(process.env.PUDDLE_STUB, 'seen'); setInterval(() => {}, 1000)",
                ),
            ],
            &Wiring {
                port,
                env: &env,
                node: &node,
                modules: &modules,
                log: &log,
                connect,
            },
        )
        .await
    }

    #[tokio::test]
    async fn the_driver_retries_a_refused_connection_and_then_reports_what_the_window_shows() {
        let modules = stub_modules();
        let mut window = stub_window(&modules, 9230, Duration::from_secs(30))
            .await
            .expect("connected after two refusals");
        let seen = window
            .attached(Duration::from_secs(5))
            .await
            .expect("attached");
        assert_eq!(
            seen,
            Attached {
                folders: vec!["api".to_owned(), "web".to_owned()],
                remote: "SSH: x".to_owned(),
                title: "t".to_owned(),
            }
        );
        assert_eq!(
            window.evaluate("1 + 1").await.expect("eval"),
            json!("1 + 1")
        );
        // The window's process got the launch's environment.
        let seen = modules.path().join("env-seen");
        puddle_test_kit::eventually(
            "the window's process ran with the env",
            Duration::from_secs(10),
            || seen.is_file(),
        )
        .await;
        let err = window.evaluate("boom").await.expect_err("throws");
        assert!(err.to_string().contains("boom"), "{err}");
        window.close().await.expect("closed");
    }

    #[tokio::test]
    async fn a_window_that_never_answers_names_the_port_and_is_stopped() {
        let modules = stub_modules();
        let err = stub_window(&modules, 1, Duration::from_millis(900))
            .await
            .expect_err("no window");
        let text = err.to_string();
        assert!(text.contains("no VS Code window on port 1"), "{text}");
        assert!(text.contains("ECONNREFUSED"), "{text}");
    }

    #[tokio::test]
    async fn the_palette_takes_the_row_with_exactly_that_label() {
        let modules = stub_modules();
        let mut window = stub_window(&modules, 9230, Duration::from_secs(30))
            .await
            .expect("connected");
        window
            .palette("Terminal: Create New Terminal (Local)")
            .await
            .expect("palette");
        let calls = window.evaluate("calls").await.expect("calls");
        let calls: Vec<&str> = calls
            .as_array()
            .expect("array")
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert_eq!(calls[0], "press F1");
        assert_eq!(calls[1], ".quick-input-widget input waitFor visible");
        assert_eq!(calls[2], "insert Terminal: Create New Terminal (Local)");
        // The row filter is an exact match with the label's regex characters escaped: a prefix of
        // another label, or a label with parentheses, picks no other row.
        assert_eq!(
            calls[3],
            r".quick-input-list .monaco-list-row .label-name filter /^\s*Terminal: Create New Terminal \(Local\)\s*$/ first click"
        );
        window.close().await.expect("closed");
    }

    #[tokio::test]
    async fn a_prompt_is_read_then_answered_with_escape_and_no_prompt_is_none() {
        let modules = stub_modules();
        let mut window = stub_window(&modules, 9230, Duration::from_secs(30))
            .await
            .expect("connected");
        let asked = window
            .dismiss_prompt(Duration::from_secs(1))
            .await
            .expect("prompt");
        assert_eq!(asked.as_deref(), Some("Git: https://github.com"));
        let calls = window.evaluate("calls").await.expect("calls");
        let calls: Vec<&str> = calls
            .as_array()
            .expect("array")
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert_eq!(
            calls,
            [
                ".quick-input-widget:visible first waitFor visible",
                "press Escape",
                ".quick-input-widget:visible first waitFor hidden"
            ]
        );
        // The stub shows no input box the second time.
        assert_eq!(
            window
                .dismiss_prompt(Duration::from_secs(1))
                .await
                .expect("no prompt"),
            None
        );
        window.close().await.expect("closed");
    }

    #[tokio::test]
    async fn a_terminal_takes_the_first_folder_when_asked_and_types_its_line() {
        let modules = stub_modules();
        let mut window = stub_window(&modules, 9230, Duration::from_secs(30))
            .await
            .expect("connected");
        window.terminal("echo one").await.expect("with a picker");
        window.terminal("echo two").await.expect("without one");
        let calls = window.evaluate("calls").await.expect("calls");
        let calls: Vec<&str> = calls
            .as_array()
            .expect("array")
            .iter()
            .filter_map(Value::as_str)
            .collect();
        let enters = calls.iter().filter(|c| **c == "press Enter").count();
        // Two lines typed, and one folder chosen in front of the first.
        assert_eq!(enters, 3, "{calls:?}");
        assert!(calls.contains(&"type echo one") && calls.contains(&"type echo two"));
        // The folder question is the input's placeholder, not text of the widget.
        assert!(
            calls.contains(
                &r#".quick-input-widget:visible input[placeholder*="working directory" i] waitFor visible"#
            ),
            "{calls:?}"
        );
        window.close().await.expect("closed");
    }

    #[tokio::test]
    async fn a_reload_marks_the_old_document_then_waits_for_a_new_one_and_then_for_the_workbench() {
        let modules = stub_modules();
        let mut window = stub_window(&modules, 9230, Duration::from_secs(30))
            .await
            .expect("connected");
        let back = window
            .reload(Duration::from_secs(5), &modules.path().join("r.png"))
            .await
            .expect("reload");
        assert_eq!(back.folders, ["api", "web"]);
        let calls = window.evaluate("calls").await.expect("calls");
        let calls: Vec<&str> = calls
            .as_array()
            .expect("array")
            .iter()
            .filter_map(Value::as_str)
            .collect();
        // The marker is set before the command's row is clicked, so a fast reload is not missed.
        let listen = calls
            .iter()
            .position(|c| *c == "mark old document")
            .expect("marks");
        let click = calls
            .iter()
            .position(|c| c.contains("Developer: Reload Window") && c.ends_with("click"))
            .expect("clicks the row");
        let waits = calls
            .iter()
            .position(|c| *c == "new document")
            .expect("waits for the new document");
        assert!(listen < click && click < waits, "{calls:?}");
        assert!(
            calls
                .last()
                .is_some_and(|c| c.starts_with("[id=\"status.host\"] filter /SSH:/")),
            "the last step waits for the remote indicator of the new window: {calls:?}"
        );
        let shot = modules.path().join("w.png");
        window.screenshot(&shot).await.expect("screenshot");
        window.close().await.expect("closed");
    }

    #[tokio::test]
    async fn a_closed_window_refuses_further_steps() {
        let modules = stub_modules();
        let window = stub_window(&modules, 9230, Duration::from_secs(30))
            .await
            .expect("connected");
        let mut window = window;
        let driver = window.driver.take().expect("driver");
        driver.quit().await.expect("driver ended");
        let err = window.evaluate("1").await.expect_err("no driver");
        assert!(err.to_string().contains("the window is closed"), "{err}");
        // Closing a window whose driver is already gone still ends cleanly.
        window.close().await.expect("closed anyway");
    }

    #[tokio::test]
    async fn the_hostile_guest_code_is_valid_javascript_that_marks_then_types_into_a_local_terminal()
     {
        let probe = r"C:\Users\x\AppData\Local\Temp\probe.txt";
        let code = local_terminal_probe("/tmp/ran", probe);
        // Valid JavaScript, run for real against a `vscode` stand-in: it writes its marker and then
        // asks for a local terminal and types the command with the Windows path intact.
        let script = format!(
            "const Module = require('node:module'); const real = Module._load; const asked = [];
             Module._load = (r, ...a) => r === 'vscode'
               ? {{ commands: {{ executeCommand: async (...c) => {{ asked.push(c); }} }} }}
               : real(r, ...a);
             const fs = require('fs'); const written = [];
             fs.writeFileSync = (f, d) => written.push([f, d]);
             eval({});
             setTimeout(() => console.log(JSON.stringify({{ asked, written: written.map((w) => w[0]) }})), 100);",
            serde_json::to_string(&code).expect("string")
        );
        let out = Node::from_env()
            .run(["-e", &script], &[], "", Duration::from_secs(30))
            .await
            .expect("node runs");
        assert!(out.success, "{}", out.stderr);
        let seen: Value = serde_json::from_str(out.stdout.trim()).expect("one line");
        assert_eq!(seen["written"], json!(["/tmp/ran"]));
        assert_eq!(
            seen["asked"][0],
            json!(["workbench.action.terminal.newLocal"])
        );
        assert_eq!(
            seen["asked"][1],
            json!([
                "workbench.action.terminal.sendSequence",
                {"text": format!("whoami > {probe}\r")}
            ])
        );
    }

    /// Unix only: the stand-ins for `code.cmd` and `Code.exe` are shell scripts.
    #[cfg(unix)]
    fn stub_release(dir: &Path, code_cmd: &str, code_exe: &str) -> VsCodeRelease {
        let app = dir.join("app");
        std::fs::create_dir_all(app.join("bin")).expect("mkdir");
        std::fs::write(dir.join("remote-ssh.vsix"), "").expect("vsix");
        for (path, body) in [
            (app.join("bin").join("code.cmd"), code_cmd),
            (app.join("Code.exe"), code_exe),
        ] {
            puddle_test_kit::write_executable(&path, &format!("#!/bin/sh\n{body}\n"));
        }
        VsCodeRelease::new(dir.to_owned()).expect("a complete release")
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn preparing_writes_the_settings_and_installs_remote_ssh_into_the_extensions_folder() {
        let dir = tempfile::tempdir().expect("tempdir");
        let seen = dir.path().join("cli-args");
        let release = stub_release(
            dir.path(),
            &format!("printf '%s\\n' \"$@\" > '{}'", seen.display()),
            "true",
        );
        let launch = VsCodeLaunch {
            user_data: dir.path().join("user"),
            extensions: dir.path().join("ext"),
            ..launch()
        };
        launch.prepare(&release).await.expect("prepared");
        let written: Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join("user/User/settings.json")).expect("settings"),
        )
        .expect("json");
        assert_eq!(written, launch.settings());
        let args = std::fs::read_to_string(&seen).expect("the tool ran");
        assert_eq!(
            args.lines().collect::<Vec<_>>(),
            [
                format!("--user-data-dir={}", dir.path().join("user").display()),
                format!("--extensions-dir={}", dir.path().join("ext").display()),
                "--install-extension".to_owned(),
                release.remote_ssh().display().to_string(),
            ]
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_profile_gets_remote_ssh_and_the_settings_of_the_default_profile() {
        let dir = tempfile::tempdir().expect("tempdir");
        let seen = dir.path().join("args");
        let release = stub_release(
            dir.path(),
            &format!("printf '%s\\n' \"$@\" > '{}'", seen.display()),
            "true",
        );
        let launch = VsCodeLaunch {
            user_data: dir.path().join("user"),
            extensions: dir.path().join("ext"),
            profile: Some("puddle-test".to_owned()),
            ..launch()
        };
        let made = dir.path().join("user/User/profiles/1bc81f32");
        std::fs::create_dir_all(&made).expect("the profile folder the first start made");
        launch.prepare_profile(&release).await.expect("prepared");
        let args = std::fs::read_to_string(&seen).expect("the tool ran");
        assert!(args.lines().any(|l| l == "--profile=puddle-test"), "{args}");
        let written: Value = serde_json::from_str(
            &std::fs::read_to_string(made.join("settings.json")).expect("settings"),
        )
        .expect("json");
        assert_eq!(written, launch.settings());
        // Without a profile there is nothing to do.
        let none = VsCodeLaunch {
            profile: None,
            ..launch
        };
        none.prepare_profile(&release).await.expect("nothing");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_package_the_tool_refuses_is_an_error_with_what_it_said() {
        let dir = tempfile::tempdir().expect("tempdir");
        let release = stub_release(
            dir.path(),
            "echo bad package; echo and more >&2; exit 1",
            "true",
        );
        let launch = VsCodeLaunch {
            user_data: dir.path().join("user"),
            extensions: dir.path().join("ext"),
            ..launch()
        };
        let err = launch.prepare(&release).await.expect_err("refused");
        let text = err.to_string();
        assert!(text.contains("installing Remote-SSH failed"), "{text}");
        assert!(
            text.contains("bad package") && text.contains("and more"),
            "{text}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_launch_starts_the_release_with_its_arguments_and_env_and_connects_the_driver() {
        let modules = stub_modules();
        let dir = tempfile::tempdir().expect("tempdir");
        let seen = dir.path().join("exe-args");
        let release = stub_release(
            dir.path(),
            "true",
            &format!(
                "printf '%s\\n' \"$@\" > '{}'; echo \"$PUDDLE_STUB_VAR\" >> '{0}'; exec sleep 30",
                seen.display()
            ),
        );
        let launch = VsCodeLaunch {
            env: vec![("PUDDLE_STUB_VAR".to_owned(), "from-launch".to_owned())],
            ..launch()
        };
        let window = launch
            .start(
                &release,
                &Node::from_env(),
                &modules.path().join("node_modules"),
                &dir.path().join("code.log"),
            )
            .await
            .expect("started and connected");
        puddle_test_kit::eventually("the program ran", Duration::from_secs(10), || {
            std::fs::read_to_string(&seen).is_ok_and(|t| t.contains("from-launch"))
        })
        .await;
        let ran = std::fs::read_to_string(&seen).expect("args");
        assert!(ran.contains("--remote-debugging-port=9230"), "{ran}");
        assert!(ran.ends_with("vscode-remote://x/y\nfrom-launch\n"), "{ran}");
        window.close().await.expect("closed");
    }
}
