// SPDX-License-Identifier: GPL-3.0-or-later
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
mod tests {
    use super::*;

    #[test]
    fn a_named_endpoint_wins_on_both_systems() {
        for windows in [true, false] {
            let got = AgentEndpoint::resolve(
                windows,
                Some(r"  \\.\pipe\pageant.alice  "),
                Some(OsStr::new("/run/agent")),
            );
            assert_eq!(got, Some(AgentEndpoint::at(r"\\.\pipe\pageant.alice")));
        }
    }

    #[test]
    fn windows_falls_back_to_the_openssh_pipe_whatever_the_environment_says() {
        for configured in [None, Some(""), Some("  ")] {
            let got = AgentEndpoint::resolve(true, configured, Some(OsStr::new("/run/agent")));
            assert_eq!(got, Some(AgentEndpoint::at(WINDOWS_DEFAULT_PIPE)));
        }
    }

    #[test]
    fn elsewhere_the_environment_names_the_socket() {
        let got =
            AgentEndpoint::resolve(false, None, Some(OsStr::new("/run/user/1000/agent.sock")));
        assert_eq!(got, Some(AgentEndpoint::at("/run/user/1000/agent.sock")));
        assert_eq!(AgentEndpoint::resolve(false, Some(" "), None), None);
        assert_eq!(
            AgentEndpoint::resolve(false, None, Some(OsStr::new(""))),
            None
        );
    }

    #[test]
    fn it_shows_its_path() {
        let e = AgentEndpoint::at("/a/b");
        assert_eq!(e.to_string(), "/a/b");
        assert_eq!(e.path(), Path::new("/a/b"));
    }
}
