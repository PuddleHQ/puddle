// SPDX-License-Identifier: AGPL-3.0-or-later
//! Errors the harness reports before or around a VM test.

use std::path::PathBuf;
use std::time::Duration;

/// Why the harness couldn't set up, run or clean up a VM test.
#[derive(Debug, thiserror::Error)]
pub enum HarnessError {
    /// A required environment variable is missing.
    #[error("{var} is not set: {hint}")]
    MissingVar {
        /// The variable.
        var: &'static str,
        /// What to set it to.
        hint: &'static str,
    },
    /// An msb variable is set in the environment and would override the harness's runtime pair.
    #[error(
        "{var} is set in the environment; unset it so the VM tests use their own runtime and home"
    )]
    AmbientMsbVar {
        /// The variable.
        var: &'static str,
    },
    /// The run prefix isn't usable as the start of a sandbox name.
    #[error(
        "invalid run prefix {value:?}: use 1-{max} lowercase letters, digits or '-', starting with a letter"
    )]
    InvalidPrefix {
        /// The rejected value, cut to 64 characters.
        value: String,
        /// The length limit.
        max: usize,
    },
    /// `PUDDLE_VM_MSB_LOG_LEVEL` isn't one of msb's log levels.
    #[error("invalid PUDDLE_VM_MSB_LOG_LEVEL {value:?}: use error, warn, info, debug or trace")]
    InvalidLogLevel {
        /// The rejected value, cut to 64 characters.
        value: String,
    },
    /// A prefixed sandbox name isn't a valid Puddle sandbox name.
    #[error("invalid sandbox name for tag {tag:?}: {reason}")]
    InvalidName {
        /// The tag the test asked for, cut to 64 characters.
        tag: String,
        /// Why the name was rejected.
        reason: String,
    },
    /// The runtime directory lacks `msb` or `libkrunfw`.
    #[error("runtime directory {dir} has no {missing}")]
    RuntimeIncomplete {
        /// The directory searched.
        dir: PathBuf,
        /// The file that wasn't found.
        missing: &'static str,
    },
    /// The harness runs on an OS msb doesn't support.
    #[error("msb has no runtime for this OS ({os})")]
    UnsupportedOs {
        /// `std::env::consts::OS`.
        os: &'static str,
    },
    /// Two tests asked for the same scratch home tag. They would share one store, so what one
    /// creates (an identity, a rule, a workspace) would show up in the other.
    #[error(
        "scratch home tag {tag:?} is already used by test {first:?}; {second:?} must take its own tag"
    )]
    ScratchTagTaken {
        /// The tag both asked for.
        tag: String,
        /// The test that claimed it first.
        first: String,
        /// The test that asked second.
        second: String,
    },
    /// A filesystem step failed.
    #[error("{action} {path}: {source}")]
    Io {
        /// What the harness was doing.
        action: &'static str,
        /// The path involved.
        path: PathBuf,
        /// The underlying error.
        source: std::io::Error,
    },
    /// An SDK call failed.
    #[error("msb SDK: {0}")]
    Sdk(#[from] microsandbox::MicrosandboxError),
    /// The folder named by `PUDDLE_VM_BROWSER_MODULES` holds no `playwright-core`.
    #[error(
        "{dir} has no playwright-core: run `npm ci --ignore-scripts` in ui and point PUDDLE_VM_BROWSER_MODULES at ui/node_modules"
    )]
    BrowserModules {
        /// The folder searched.
        dir: PathBuf,
    },
    /// The folder named by `PUDDLE_VSCODE_DIR` is not a release `ci/fetch-vscode.sh` made.
    #[error(
        "{dir} has no app/Code.exe and remote-ssh.vsix: make it with `ci/fetch-vscode.sh <dir>` and point PUDDLE_VSCODE_DIR at it"
    )]
    VsCodeRelease {
        /// The folder searched.
        dir: PathBuf,
    },
    /// The browser client could not load a page or read its result.
    #[error("browser client: {detail}")]
    Browser {
        /// What went wrong, with the page's console and errors when it got that far.
        detail: String,
    },
    /// Creating a sandbox from an image failed or ran out of time. The image comes from a public
    /// registry (a live third party): this variant names the registry so a red run reads as an
    /// outage of that host, not as a Puddle fault.
    #[error(
        "creating a sandbox from {image} needs {registry} (a live third-party registry; an outage there fails this test): {detail}"
    )]
    ImagePull {
        /// The image reference.
        image: String,
        /// The registry host the reference pulls from.
        registry: String,
        /// What the SDK or the clock reported.
        detail: String,
    },
    /// msb's record of a sandbox names no VM process.
    #[error("msb records no VM process for sandbox {name}: it is not running")]
    NoVmProcess {
        /// The sandbox name.
        name: String,
    },
    /// Freezing or thawing a VM's process failed.
    #[error("freezing or thawing process {pid}: {detail}")]
    Freeze {
        /// The process id.
        pid: u32,
        /// The command's exit status and output.
        detail: String,
    },
    /// A step took longer than its budget.
    #[error("{what} took longer than {limit:?}")]
    Timeout {
        /// The step.
        what: &'static str,
        /// Its budget.
        limit: Duration,
    },
    /// A node process did not end within its budget (it was killed). Carries what it printed and
    /// how far it got, so a hang on a CI runner says where it stood.
    #[error(
        "node process took longer than {limit:?} (pid {pid:?}, started after {spawn_took:?}): {stage}\nstdout: {stdout:?}\nstderr: {stderr:?}"
    )]
    NodeHung {
        /// Its budget.
        limit: Duration,
        /// Its process id, when the OS gave one.
        pid: Option<u32>,
        /// How long `spawn` took to return.
        spawn_took: Duration,
        /// Which steps had finished: input written, process exited, each pipe closed.
        stage: String,
        /// What it had printed to standard output, lossily decoded.
        stdout: String,
        /// What it had printed to standard error, lossily decoded.
        stderr: String,
    },
}

impl HarnessError {
    pub(crate) fn io(
        action: &'static str,
        path: impl Into<PathBuf>,
        source: std::io::Error,
    ) -> Self {
        Self::Io {
            action,
            path: path.into(),
            source,
        }
    }
}

/// Cuts a rejected value for an error message, so a hostile or huge value can't flood a log.
pub(crate) fn clip(value: &str) -> String {
    value.chars().take(64).collect()
}

#[cfg(test)]
mod tests;
