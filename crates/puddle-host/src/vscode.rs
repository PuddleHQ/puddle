// SPDX-License-Identifier: GPL-3.0-or-later
//! "Open in VS Code": starts the desktop VS Code on a workspace's ssh host entry with its own
//! `code` command, so Remote-SSH does the rest (connecting through Puddle's bridge, installing
//! its server in the guest).

use std::ffi::OsString;
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
    [flag.to_owned(), target.remote_uri()]
}

#[cfg(test)]
mod tests;
