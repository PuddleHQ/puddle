// SPDX-License-Identifier: GPL-3.0-or-later
//! "Open in VS Code": starts the desktop VS Code on a workspace's ssh host entry with its own
//! `code` command, so Remote-SSH does the rest (connecting through Puddle's bridge, installing
//! its server in the guest).

use std::ffi::OsString;
use std::fmt::Write as _;
use std::process::{Command, Stdio};
use std::time::Duration;

use futures_util::future::BoxFuture;
use puddle_api::{DesktopTarget, LaunchError, Launcher, WorkspaceRecord};

/// How long `code` is given to say it failed. It normally hands the request to the running
/// VS Code and returns at once; one that is still running after this is VS Code starting up.
const PATIENCE: Duration = Duration::from_secs(10);

/// A [`Launcher`] that runs VS Code's `code` command.
#[derive(Debug, Clone)]
pub struct VsCodeLauncher {
    program: OsString,
    patience: Duration,
}

impl Default for VsCodeLauncher {
    fn default() -> Self {
        Self::new()
    }
}

impl VsCodeLauncher {
    /// The launcher for the `code` command on the search path (`code.cmd` on Windows, which is
    /// how VS Code's installer puts it there).
    #[must_use]
    pub fn new() -> Self {
        Self::with_program(if cfg!(windows) { "code.cmd" } else { "code" })
    }

    /// The launcher for another command that takes `code`'s arguments.
    #[must_use]
    pub fn with_program(program: impl Into<OsString>) -> Self {
        Self {
            program: program.into(),
            patience: PATIENCE,
        }
    }

    /// The same launcher, giving the command `patience` to fail before it counts as started.
    #[must_use]
    pub fn with_patience(mut self, patience: Duration) -> Self {
        self.patience = patience;
        self
    }

    async fn run(&self, target: &DesktopTarget) -> Result<(), LaunchError> {
        let mut command = Command::new(&self.program);
        command
            .args(arguments(target))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt as _;
            // No console window for the command that starts the editor.
            command.creation_flags(0x0800_0000);
        }
        let child = command.spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                LaunchError::new(format!(
                    "VS Code's `{}` command was not found. Install VS Code and its shell \
                     command (in VS Code: Shell Command: Install 'code' command in PATH), or \
                     connect with `ssh {}`",
                    self.program.to_string_lossy(),
                    target.ssh_host
                ))
            } else {
                LaunchError::new(format!("could not start VS Code: {e}"))
            }
        })?;
        let waiting = tokio::task::spawn_blocking(move || child.wait_with_output());
        match tokio::time::timeout(self.patience, waiting).await {
            // Still running: VS Code is starting up.
            Err(_) => Ok(()),
            Ok(Ok(Ok(output))) if output.status.success() => Ok(()),
            Ok(Ok(Ok(output))) => Err(LaunchError::new(format!(
                "VS Code did not open: its command ended with {}{}",
                output.status,
                one_line(&String::from_utf8_lossy(&output.stderr))
            ))),
            Ok(Ok(Err(e))) => Err(LaunchError::new(format!("could not wait for VS Code: {e}"))),
            Ok(Err(e)) => Err(LaunchError::new(format!("could not wait for VS Code: {e}"))),
        }
    }
}

impl Launcher for VsCodeLauncher {
    fn open_desktop<'a>(
        &'a self,
        _workspace: &'a WorkspaceRecord,
        target: &'a DesktopTarget,
    ) -> BoxFuture<'a, Result<(), LaunchError>> {
        Box::pin(self.run(target))
    }
}

/// `": <stderr>"` as one short line, or nothing when the command said nothing.
fn one_line(text: &str) -> String {
    let clean: String = text
        .chars()
        .filter(|c| !c.is_control() || c.is_whitespace())
        .collect();
    let line: String = clean
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(300)
        .collect();
    if line.is_empty() {
        String::new()
    } else {
        format!(": {line}")
    }
}

/// The arguments that open `target`: `--folder-uri` or `--file-uri` with a `vscode-remote` URI on
/// the workspace's ssh host.
pub(crate) fn arguments(target: &DesktopTarget) -> [String; 2] {
    let flag = if target.is_workspace_file {
        "--file-uri"
    } else {
        "--folder-uri"
    };
    let mut path = String::new();
    for byte in target.path.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'_' | b'.' | b'~') {
            path.push(char::from(byte));
        } else {
            let _ = write!(path, "%{byte:02X}");
        }
    }
    [
        flag.to_owned(),
        format!("vscode-remote://ssh-remote+{}{path}", target.ssh_host),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(path: &str, file: bool) -> DesktopTarget {
        DesktopTarget {
            ssh_host: "puddle-web".into(),
            path: path.into(),
            is_workspace_file: file,
        }
    }

    #[test]
    fn a_folder_and_a_multi_root_file_open_through_the_ssh_host() {
        assert_eq!(
            arguments(&target("/workspaces/abc/app", false)),
            [
                "--folder-uri",
                "vscode-remote://ssh-remote+puddle-web/workspaces/abc/app"
            ]
        );
        assert_eq!(
            arguments(&target(
                "/workspaces/abc/.puddle/editor/abc.code-workspace",
                true
            )),
            [
                "--file-uri",
                "vscode-remote://ssh-remote+puddle-web/workspaces/abc/.puddle/editor/abc.code-workspace"
            ]
        );
    }

    #[test]
    fn a_path_is_percent_encoded_so_it_cannot_change_the_uri() {
        let [_, uri] = arguments(&target("/w/a b?x#y&z%", false));
        assert_eq!(
            uri,
            "vscode-remote://ssh-remote+puddle-web/w/a%20b%3Fx%23y%26z%25"
        );
    }

    #[test]
    fn the_default_launcher_runs_the_code_command() {
        let launcher = VsCodeLauncher::default();
        let expected = if cfg!(windows) { "code.cmd" } else { "code" };
        assert_eq!(launcher.program, expected);
    }

    #[test]
    fn stderr_is_one_short_line() {
        assert_eq!(one_line(""), "");
        assert_eq!(one_line("  a\n\tb \u{7} c "), ": a b c");
        assert_eq!(one_line(&"x".repeat(1000)).len(), 302);
    }

    #[cfg(unix)]
    mod commands {
        use std::os::unix::fs::PermissionsExt as _;

        use super::*;

        fn script(dir: &std::path::Path, body: &str) -> VsCodeLauncher {
            let path = dir.join("code");
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            VsCodeLauncher::with_program(path).with_patience(Duration::from_millis(1500))
        }

        fn record() -> WorkspaceRecord {
            WorkspaceRecord::new(
                puddle_types::WorkspaceId::new("0123456789abcdef0123456789abcdef").unwrap(),
                puddle_types::WorkspaceName::new("web").unwrap(),
                Vec::new(),
            )
        }

        #[tokio::test]
        async fn the_command_gets_the_uri_and_its_success_is_success() {
            let dir = tempfile::tempdir().unwrap();
            let out = dir.path().join("args");
            let launcher = script(
                dir.path(),
                &format!("printf '%s\\n' \"$@\" > '{}'", out.display()),
            );
            launcher
                .open_desktop(&record(), &target("/workspaces/x", false))
                .await
                .unwrap();
            assert_eq!(
                std::fs::read_to_string(out).unwrap(),
                "--folder-uri\nvscode-remote://ssh-remote+puddle-web/workspaces/x\n"
            );
        }

        #[tokio::test]
        async fn a_command_that_fails_says_why() {
            let dir = tempfile::tempdir().unwrap();
            let launcher = script(dir.path(), "echo 'no display' >&2; exit 3");
            let err = launcher
                .open_desktop(&record(), &target("/w", false))
                .await
                .unwrap_err()
                .to_string();
            assert!(
                err.contains("exit status: 3") && err.contains("no display"),
                "{err}"
            );
        }

        #[tokio::test]
        async fn a_command_that_keeps_running_counts_as_started() {
            let dir = tempfile::tempdir().unwrap();
            let launcher =
                script(dir.path(), "exec sleep 1").with_patience(Duration::from_millis(100));
            launcher
                .open_desktop(&record(), &target("/w", false))
                .await
                .unwrap();
        }

        #[tokio::test]
        async fn a_missing_command_names_the_fix_and_the_host_to_open_by_hand() {
            let launcher = VsCodeLauncher::with_program("/nonexistent/code");
            let err = launcher
                .open_desktop(&record(), &target("/w", false))
                .await
                .unwrap_err()
                .to_string();
            assert!(
                err.contains("not found") && err.contains("puddle-web"),
                "{err}"
            );
        }

        #[tokio::test]
        async fn a_command_that_cannot_be_started_says_so() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("code");
            std::fs::write(&path, "#!/bin/sh\n").unwrap();
            // Not executable.
            let launcher = VsCodeLauncher::with_program(path);
            let err = launcher
                .open_desktop(&record(), &target("/w", false))
                .await
                .unwrap_err()
                .to_string();
            assert!(err.starts_with("could not start VS Code"), "{err}");
        }
    }

    #[cfg(windows)]
    mod batch_file {
        use super::*;

        /// A `code.cmd` that writes the arguments it got, one per line, to a file beside it.
        fn stub(dir: &std::path::Path) -> (VsCodeLauncher, std::path::PathBuf) {
            let seen = dir.join("seen.txt");
            let path = dir.join("code.cmd");
            std::fs::write(
                &path,
                format!(
                    "@echo off\r\n>\"{0}\" echo %1\r\n>>\"{0}\" echo %2\r\n",
                    seen.display()
                ),
            )
            .unwrap();
            (VsCodeLauncher::with_program(path), seen)
        }

        fn record() -> WorkspaceRecord {
            WorkspaceRecord::new(
                puddle_types::WorkspaceId::new("0123456789abcdef0123456789abcdef").unwrap(),
                puddle_types::WorkspaceName::new("web").unwrap(),
                Vec::new(),
            )
        }

        #[tokio::test]
        async fn a_batch_file_gets_the_uri_of_a_path_full_of_cmd_metacharacters_unchanged() {
            // A folder with a space in the program's own path too: `cmd` has to find the file.
            let dir = tempfile::tempdir().unwrap();
            let spaced = dir.path().join("Program Files");
            std::fs::create_dir(&spaced).unwrap();
            let (launcher, seen) = stub(&spaced);
            let path = "/workspaces/a b%PATH%&calc^x|y<z>(1)!q";
            launcher
                .open_desktop(&record(), &target(path, false))
                .await
                .unwrap();
            let [flag, uri] = arguments(&target(path, false));
            let got = std::fs::read_to_string(seen).unwrap();
            // `%1` keeps the quotes Rust puts round an argument `cmd` could misread; the value
            // inside them is the URI, byte for byte.
            let got: Vec<&str> = got.lines().map(|l| l.trim_matches('"')).collect();
            assert_eq!(got, [flag.as_str(), uri.as_str()]);
            assert!(
                !uri.contains(['&', '^', ' ', '|', '<', '>', '(', '!', '"']),
                "{uri}"
            );
        }

        #[tokio::test]
        async fn a_batch_file_that_fails_says_why() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("code.cmd");
            std::fs::write(&path, "@echo off\r\necho no display 1>&2\r\nexit /b 3\r\n").unwrap();
            let err = VsCodeLauncher::with_program(path)
                .open_desktop(&record(), &target("/w", false))
                .await
                .unwrap_err()
                .to_string();
            assert!(
                err.contains("exit code: 3") && err.contains("no display"),
                "{err}"
            );
        }
    }
}
