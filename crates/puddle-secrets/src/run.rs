// SPDX-License-Identifier: AGPL-3.0-or-later
//! Running the fixed invocations: resolved paths, no console window, never a prompt.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use zeroize::Zeroizing;

use crate::error::{SourceError, Tool};

/// How long a tool may take before it counts as stuck (most likely on a sign-in window).
pub(crate) const TOOL_TIMEOUT: Duration = Duration::from_secs(10);

/// How long the Azure CLI may take: a cold start of its Python runtime is slow, notably on Windows.
pub(crate) const AZ_TIMEOUT: Duration = Duration::from_secs(20);

/// Where `gh`, `git` and `az` are, resolved once. A missing one stays `None` and every source that
/// needs it answers [`SourceError::ToolMissing`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolPaths {
    gh: Option<PathBuf>,
    git: Option<PathBuf>,
    az: Option<PathBuf>,
}

impl ToolPaths {
    /// Paths given directly (tests, or a setting).
    #[must_use]
    pub fn new(gh: Option<PathBuf>, git: Option<PathBuf>) -> Self {
        Self { gh, git, az: None }
    }

    /// The same paths with `az` at `az` (tests, or a setting).
    #[must_use]
    pub fn with_az(mut self, az: Option<PathBuf>) -> Self {
        self.az = az;
        self
    }

    /// Looks `gh`, `git` and `az` up in the process's `PATH`.
    #[must_use]
    pub fn resolve() -> Self {
        Self::from_search_path(std::env::var_os("PATH").as_deref().unwrap_or_default())
    }

    /// Looks `gh`, `git` and `az` up in `search_path` (a `PATH`-style list). Relative entries are
    /// skipped, so the working directory is never searched; on Windows only `.exe` files count for
    /// `gh` and `git`, never a `.cmd` or `.bat` script. `az` is the exception: it ships as `az.cmd`,
    /// and its one validated argument has no character a batch file would re-read.
    #[must_use]
    pub fn from_search_path(search_path: &OsStr) -> Self {
        Self {
            gh: find("gh", search_path),
            git: find("git", search_path),
            az: find_file(if cfg!(windows) { "az.cmd" } else { "az" }, search_path),
        }
    }

    pub(crate) fn get(&self, tool: Tool) -> Result<&Path, SourceError> {
        let path = match tool {
            Tool::Gh => &self.gh,
            Tool::Git => &self.git,
            Tool::Az => &self.az,
        };
        path.as_deref().ok_or(SourceError::ToolMissing(tool))
    }
}

fn find(name: &str, search_path: &OsStr) -> Option<PathBuf> {
    let file = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_owned()
    };
    find_file(&file, search_path)
}

fn find_file(file: &str, search_path: &OsStr) -> Option<PathBuf> {
    std::env::split_paths(search_path)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(file))
        .find(|candidate| is_executable(candidate))
}

/// The two bodies have their own names so that a mutation exclusion can name the one a Linux run
/// never compiles.
fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        has_execute_bit(path)
    }
    #[cfg(not(unix))]
    {
        exists_as_file(path)
    }
}

#[cfg(unix)]
fn has_execute_bit(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn exists_as_file(path: &Path) -> bool {
    path.is_file()
}

/// What a finished tool left behind.
pub(crate) struct Output {
    pub(crate) success: bool,
    /// Standard output; wiped on drop because it may hold a token.
    pub(crate) stdout: Zeroizing<Vec<u8>>,
    /// Standard error, only when the caller asked for it ([`run_capturing`]); empty otherwise.
    pub(crate) stderr: Zeroizing<Vec<u8>>,
}

/// Whether the user is at the screen to answer what a tool opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Prompts {
    /// Every prompt, window and askpass off: a request from a workspace.
    Never,
    /// Git Credential Manager may open its own window or browser: the user just clicked Sign in.
    /// `gh` stays non-interactive (it prints a code instead of asking).
    UserPresent,
}

/// A command with the environment every fixed invocation gets: no console window and, unless the
/// user is present, no prompt of any kind.
#[expect(
    clippy::disallowed_methods,
    reason = "the working folder only has to exist and be neutral; no file is made in it"
)]
pub(crate) fn command(program: &Path, prompts: Prompts) -> Command {
    let mut cmd = Command::new(program);
    cmd.current_dir(std::env::temp_dir())
        .kill_on_drop(true)
        // Git and Git Credential Manager: no terminal prompt and no askpass program; no GUI
        // prompt either unless the user is present.
        .env(
            "GCM_INTERACTIVE",
            match prompts {
                Prompts::Never => "never",
                Prompts::UserPresent => "true",
            },
        )
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "")
        .env("SSH_ASKPASS", "")
        // gh: no prompt, and a token in the environment must not stand in for the named account.
        .env("GH_PROMPT_DISABLED", "1")
        .env("GH_NO_UPDATE_NOTIFIER", "1")
        // az: plain text, so its error lines can be told apart.
        .env("AZURE_CORE_NO_COLOR", "1")
        .env_remove("GH_TOKEN")
        .env_remove("GITHUB_TOKEN")
        .env_remove("GH_ENTERPRISE_TOKEN")
        .env_remove("GITHUB_ENTERPRISE_TOKEN");
    #[cfg(windows)]
    {
        // CREATE_NO_WINDOW: a console tool must not flash a window on the user's desktop.
        cmd.creation_flags(0x0800_0000);
    }
    cmd
}

/// What starting a tool failed with.
pub(crate) fn spawn_error(tool: Tool, err: &std::io::Error) -> SourceError {
    tracing::debug!(tool = tool.name(), kind = ?err.kind(), "could not start the tool");
    if err.kind() == std::io::ErrorKind::NotFound {
        SourceError::ToolMissing(tool)
    } else {
        SourceError::CouldNotRun(tool)
    }
}

/// Runs `program` with fixed `args`, `stdin` and the prompt-suppressing environment. Never inherits
/// a console, a terminal or a prompt. Standard error is discarded.
pub(crate) async fn run(
    tool: Tool,
    program: &Path,
    args: &[&str],
    stdin: Option<&[u8]>,
    timeout: Duration,
) -> Result<Output, SourceError> {
    run_with(Prompts::Never, tool, program, args, stdin, timeout).await
}

/// [`run`], with `prompts` saying whether the user is present.
pub(crate) async fn run_with(
    prompts: Prompts,
    tool: Tool,
    program: &Path,
    args: &[&str],
    stdin: Option<&[u8]>,
    timeout: Duration,
) -> Result<Output, SourceError> {
    run_inner(prompts, false, tool, program, args, stdin, timeout).await
}

/// [`run`], keeping standard error for the caller to classify a failure by. The caller reads it
/// and drops it: it never goes to a log or an event.
pub(crate) async fn run_capturing(
    tool: Tool,
    program: &Path,
    args: &[&str],
    timeout: Duration,
) -> Result<Output, SourceError> {
    run_inner(Prompts::Never, true, tool, program, args, None, timeout).await
}

async fn run_inner(
    prompts: Prompts,
    capture_stderr: bool,
    tool: Tool,
    program: &Path,
    args: &[&str],
    stdin: Option<&[u8]>,
    timeout: Duration,
) -> Result<Output, SourceError> {
    let mut cmd = command(program, prompts);
    cmd.args(args)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(if capture_stderr {
            Stdio::piped()
        } else {
            Stdio::null()
        });
    let mut child = cmd.spawn().map_err(|err| spawn_error(tool, &err))?;
    if let (Some(bytes), Some(mut pipe)) = (stdin, child.stdin.take()) {
        // A tool that exits before reading its input is not an error here: its status says so.
        let _ = pipe.write_all(bytes).await;
    }
    let mut stdout = child.stdout.take().ok_or(SourceError::CouldNotRun(tool))?;
    let mut stderr_pipe = child.stderr.take();
    let read = async {
        // Both pipes are drained together so a tool that fills one while we wait on the other cannot stall.
        let (bytes, errors) = tokio::join!(read_wiping(&mut stdout), async {
            match stderr_pipe.as_mut() {
                Some(pipe) => read_wiping(pipe).await,
                None => Ok(Zeroizing::new(Vec::new())),
            }
        });
        let status = child.wait().await?;
        Ok::<_, std::io::Error>((status, bytes?, errors?))
    };
    let answered = tokio::time::timeout(timeout, read).await;
    let (status, stdout, stderr) = answered
        .map_err(|_| SourceError::Timeout(tool))?
        .map_err(|_| SourceError::CouldNotRun(tool))?;
    Ok(Output {
        success: status.success(),
        stdout,
        stderr,
    })
}

/// First size of the buffer a tool's output is read into; a token answer fits in it.
const FIRST_READ: usize = 4096;

/// Reads `reader` to its end into a buffer that is overwritten when it drops. When the buffer is
/// full the next one is made first and the old one is overwritten after the copy, so no
/// reallocation frees an unwiped copy of the output.
async fn read_wiping<R: AsyncRead + Unpin>(reader: &mut R) -> std::io::Result<Zeroizing<Vec<u8>>> {
    let mut buf = Zeroizing::new(Vec::with_capacity(FIRST_READ));
    loop {
        if buf.len() == buf.capacity() {
            let mut bigger = Zeroizing::new(Vec::with_capacity(buf.capacity() * 2));
            bigger.extend_from_slice(&buf);
            // After the swap `bigger` holds the full old buffer and wipes it as it drops.
            std::mem::swap(&mut buf, &mut bigger);
        }
        if reader.read_buf(&mut *buf).await? == 0 {
            return Ok(buf);
        }
    }
}

#[cfg(test)]
mod tests;
