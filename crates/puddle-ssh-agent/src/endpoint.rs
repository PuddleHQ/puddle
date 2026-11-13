// SPDX-License-Identifier: AGPL-3.0-or-later
//! Where the agent listens.

use std::ffi::OsStr;
use std::fmt;
use std::path::{Path, PathBuf};

/// The pipe the OpenSSH Authentication Agent serves on Windows. 1Password and Bitwarden serve the
/// same name, so one default covers all three.
pub const WINDOWS_DEFAULT_PIPE: &str = r"\\.\pipe\openssh-ssh-agent";

/// The named pipe (Windows) or Unix socket (elsewhere) an agent listens on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentEndpoint {
    path: PathBuf,
}

impl AgentEndpoint {
    /// The endpoint at `path`: a pipe name such as `\\.\pipe\pageant.alice.0123` on Windows, a
    /// socket path elsewhere.
    #[must_use]
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// The pipe or socket path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The endpoint Puddle asks on this machine: the one the settings name, else on Windows
    /// [`WINDOWS_DEFAULT_PIPE`] and elsewhere the `SSH_AUTH_SOCK` of this process. `None` when the
    /// settings name none and there is no `SSH_AUTH_SOCK`.
    #[must_use]
    pub fn for_host(configured: Option<&str>) -> Option<Self> {
        Self::resolve(
            cfg!(windows),
            configured,
            std::env::var_os("SSH_AUTH_SOCK").as_deref(),
        )
    }

    /// [`Self::for_host`] with the OS and the environment given, so every rule is tested on every
    /// platform. A blank setting counts as unset.
    #[must_use]
    pub fn resolve(
        windows: bool,
        configured: Option<&str>,
        auth_sock: Option<&OsStr>,
    ) -> Option<Self> {
        if let Some(named) = configured.map(str::trim).filter(|s| !s.is_empty()) {
            return Some(Self::at(named));
        }
        if windows {
            return Some(Self::at(WINDOWS_DEFAULT_PIPE));
        }
        auth_sock.filter(|s| !s.is_empty()).map(Self::at)
    }
}

impl fmt::Display for AgentEndpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.path.display().fmt(f)
    }
}

#[cfg(test)]
mod tests;
