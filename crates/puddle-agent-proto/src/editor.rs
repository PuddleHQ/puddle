// SPDX-License-Identifier: AGPL-3.0-or-later
//! How the host tells the agent to run the browser editor's server (code-server).
//!
//! The host sets four environment variables on the agent ([`EditorServer::env`]); the agent reads
//! them back with [`EditorServer::from_lookup`]. Without [`ENV_DIR`] the agent starts no server.
//! One definition keeps the names and the limits the same on both sides.
//!
//! | Variable | |
//! |---|---|
//! | [`ENV_DIR`] | the read-only folder with the code-server release; its presence switches the server on |
//! | [`ENV_PORT`] | the port on the guest's loopback |
//! | [`ENV_DATA`] | where user data and extensions live (on the workspace volume) |
//! | [`ENV_GRACE`] | the reconnection grace in seconds |
//! | [`ENV_FOLDER`] | the folder the editor opens (the workspace volume) |
//! | [`ENV_WORKSPACE_FILE`] | the multi-root file the editor opens instead when it exists and lists folders (optional) |
//! | [`ENV_KIND`] | which server: unset or `code-server`, or `microsoft` (Microsoft's own, which the agent downloads) |
//! | [`ENV_TELEMETRY`] | `1` or `0`: whether Microsoft's server may send telemetry (only with `microsoft`) |
//! | [`ENV_AUTO_UPDATE`] | `1` or `0`: whether the agent looks for newer builds of Microsoft's server (only with `microsoft`; unset is `1`) |
//!
//! For Microsoft's server [`ENV_DIR`] is the folder its builds are unpacked into, one folder per
//! build, on the workspace volume: the host mounts no release, the agent fetches one.
//!
//! ```
//! use puddle_agent_proto::editor::EditorServer;
//! let server = EditorServer::new("/workspaces/acme/.puddle/code-server", "/workspaces/acme", 300);
//! let env = server.env();
//! let back = EditorServer::from_lookup(|k| {
//!     env.iter().find(|(name, _)| *name == k).map(|(_, v)| v.clone())
//! });
//! assert_eq!(back, Ok(Some(server)));
//! ```

use std::fmt;

use puddle_types::SessionUser;

/// Why the editor server cannot run in a guest whose image has musl and no glibc (for example
/// Alpine). The agent sends it as the editor's install problem and answers the host's editor
/// requests with it; the host words the problem for the user from it.
pub const NEEDS_GLIBC: &str =
    "the browser editor needs an image with glibc; this image uses musl (for example Alpine)";

/// The folder with the code-server release, as the guest mounts it.
pub const GUEST_DIR: &str = "/puddle/code-server";

/// The release folder variable.
pub const ENV_DIR: &str = "PUDDLE_AGENT_EDITOR_DIR";
/// The port variable.
pub const ENV_PORT: &str = "PUDDLE_AGENT_EDITOR_PORT";
/// The data folder variable.
pub const ENV_DATA: &str = "PUDDLE_AGENT_EDITOR_DATA";
/// The reconnection grace variable (seconds).
pub const ENV_GRACE: &str = "PUDDLE_AGENT_EDITOR_GRACE";
/// The folder the editor opens.
pub const ENV_FOLDER: &str = "PUDDLE_AGENT_EDITOR_FOLDER";
/// The multi-root file the editor opens instead of the folder, when the file exists and lists
/// a folder. Unset: the editor always opens the folder.
pub const ENV_WORKSPACE_FILE: &str = "PUDDLE_AGENT_EDITOR_WORKSPACE_FILE";

/// The server variable.
pub const ENV_KIND: &str = "PUDDLE_AGENT_EDITOR_KIND";
/// The telemetry variable of Microsoft's server.
pub const ENV_TELEMETRY: &str = "PUDDLE_AGENT_EDITOR_TELEMETRY";

/// The automatic-update variable of Microsoft's server.
pub const ENV_AUTO_UPDATE: &str = "PUDDLE_AGENT_EDITOR_AUTO_UPDATE";

/// The user the server runs as. Unset: root, the agent's own user.
pub const ENV_USER: &str = "PUDDLE_AGENT_EDITOR_USER";

/// The folder below the data folder that holds Microsoft's builds, one folder per build.
pub const MICROSOFT_BUILDS: &str = "servers";

/// Where the agent writes the server's password (owner-only; the host reads it to sign the
/// window in). It lives in `/run`: a new random value for each boot of the sandbox, kept while
/// the sandbox runs, so a restarted server accepts the sessions the old one signed in.
pub const PASSWORD_FILE: &str = "/run/puddle/editor-password";

/// The default port: on the guest's loopback, above the ranges dev servers use.
pub const DEFAULT_PORT: u16 = 41_337;

/// Shortest reconnection grace the settings allow, in seconds.
pub const MIN_GRACE_SECS: u32 = 30;
/// Longest reconnection grace the settings allow, in seconds (24 h).
pub const MAX_GRACE_SECS: u32 = 86_400;

/// Which server the agent runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ServerKind {
    /// code-server, the release the host mounts read-only at [`EditorServer::dir`].
    #[default]
    CodeServer,
    /// Microsoft's VS Code server: the agent downloads a build into [`EditorServer::dir`].
    Microsoft {
        /// Whether the server may send telemetry to Microsoft (the user's choice in the popup).
        telemetry: bool,
        /// Whether the agent looks for newer builds at start and daily (the user's setting).
        auto_update: bool,
    },
}

/// How to run the editor server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorServer {
    /// Which server.
    pub kind: ServerKind,
    /// The code-server release folder in the guest ([`GUEST_DIR`] unless a test moves it).
    pub dir: String,
    /// The port on 127.0.0.1.
    pub port: u16,
    /// The folder holding `user-data/` and `extensions/`.
    pub data: String,
    /// The folder the editor opens.
    pub folder: String,
    /// The multi-root file the editor opens instead of [`Self::folder`] at each start of the
    /// server where the file exists and lists a folder. The host writes it at an attach, so the
    /// server only finds it at a start that comes after one.
    pub workspace_file: Option<String>,
    /// The reconnection grace in seconds ([`MIN_GRACE_SECS`] to [`MAX_GRACE_SECS`]).
    pub grace_secs: u32,
    /// The user the server runs as: the workspace's session user, so its terminals are that
    /// user's.
    pub user: SessionUser,
}

impl EditorServer {
    /// A server from [`GUEST_DIR`] on [`DEFAULT_PORT`], keeping its data in `data`, opening
    /// `folder`.
    #[must_use]
    pub fn new(data: impl Into<String>, folder: impl Into<String>, grace_secs: u32) -> Self {
        Self {
            kind: ServerKind::CodeServer,
            dir: GUEST_DIR.to_owned(),
            port: DEFAULT_PORT,
            data: data.into(),
            folder: folder.into(),
            workspace_file: None,
            grace_secs,
            user: SessionUser::Root,
        }
    }

    /// Microsoft's server, kept in `data` (its builds in [`MICROSOFT_BUILDS`] below it), opening
    /// `folder`.
    #[must_use]
    pub fn microsoft(
        data: impl Into<String>,
        folder: impl Into<String>,
        grace_secs: u32,
        telemetry: bool,
        auto_update: bool,
    ) -> Self {
        let mut server = Self::new(data, folder, grace_secs);
        server.dir = format!("{}/{MICROSOFT_BUILDS}", server.data);
        server.kind = ServerKind::Microsoft {
            telemetry,
            auto_update,
        };
        server
    }

    /// The same server, opening `file` instead of the folder when it holds a multi-root list.
    #[must_use]
    pub fn with_workspace_file(mut self, file: impl Into<String>) -> Self {
        self.workspace_file = Some(file.into());
        self
    }

    /// The same server, run as `user`.
    #[must_use]
    pub fn with_user(mut self, user: SessionUser) -> Self {
        self.user = user;
        self
    }

    /// The variables that tell the agent to run this server.
    #[must_use]
    pub fn env(&self) -> Vec<(&'static str, String)> {
        let mut env = vec![
            (ENV_DIR, self.dir.clone()),
            (ENV_PORT, self.port.to_string()),
            (ENV_DATA, self.data.clone()),
            (ENV_FOLDER, self.folder.clone()),
            (ENV_GRACE, self.grace_secs.to_string()),
        ];
        if let Some(file) = &self.workspace_file {
            env.push((ENV_WORKSPACE_FILE, file.clone()));
        }
        if !self.user.is_root() {
            env.push((ENV_USER, self.user.name().to_owned()));
        }
        if let ServerKind::Microsoft {
            telemetry,
            auto_update,
        } = self.kind
        {
            env.push((ENV_KIND, "microsoft".to_owned()));
            env.push((ENV_TELEMETRY, if telemetry { "1" } else { "0" }.to_owned()));
            env.push((
                ENV_AUTO_UPDATE,
                if auto_update { "1" } else { "0" }.to_owned(),
            ));
        }
        env
    }

    /// Reads the settings through `get`: `None` when [`ENV_DIR`] is unset (no server).
    ///
    /// # Errors
    ///
    /// [`EditorEnvError`] for a variable that is set but unusable, or [`ENV_DATA`] or
    /// [`ENV_FOLDER`] missing while [`ENV_DIR`] is set.
    pub fn from_lookup(
        get: impl Fn(&str) -> Option<String>,
    ) -> Result<Option<Self>, EditorEnvError> {
        let Some(dir) = get(ENV_DIR) else {
            return Ok(None);
        };
        let path = |var: &'static str, value: String| {
            if value.starts_with('/') && !value.contains(['\0', '\n']) && !value.contains("/../") {
                Ok(value)
            } else {
                Err(EditorEnvError::new(var, "expected an absolute path"))
            }
        };
        let required = |var: &'static str| {
            get(var).ok_or_else(|| EditorEnvError::new(var, "must be set with the release folder"))
        };
        let port = match get(ENV_PORT) {
            None => DEFAULT_PORT,
            Some(v) => v
                .parse::<u16>()
                .ok()
                .filter(|p| *p >= 1024)
                .ok_or_else(|| {
                    EditorEnvError::new(ENV_PORT, "expected a port from 1024 to 65535")
                })?,
        };
        let grace_secs = match get(ENV_GRACE) {
            None => 300,
            Some(v) => v
                .parse::<u32>()
                .ok()
                .filter(|s| (MIN_GRACE_SECS..=MAX_GRACE_SECS).contains(s))
                .ok_or_else(|| {
                    EditorEnvError::new(ENV_GRACE, "expected seconds from 30 to 86400")
                })?,
        };
        let kind = match get(ENV_KIND).as_deref() {
            None | Some("code-server") => {
                for var in [ENV_TELEMETRY, ENV_AUTO_UPDATE] {
                    if get(var).is_some() {
                        return Err(EditorEnvError::new(
                            var,
                            "only Microsoft's server has this setting",
                        ));
                    }
                }
                ServerKind::CodeServer
            }
            Some("microsoft") => ServerKind::Microsoft {
                telemetry: match get(ENV_TELEMETRY).as_deref() {
                    None | Some("0") => false,
                    Some("1") => true,
                    Some(_) => return Err(EditorEnvError::new(ENV_TELEMETRY, "expected 0 or 1")),
                },
                // An older host sends none: updates are on unless the user turned them off.
                auto_update: match get(ENV_AUTO_UPDATE).as_deref() {
                    None | Some("1") => true,
                    Some("0") => false,
                    Some(_) => {
                        return Err(EditorEnvError::new(ENV_AUTO_UPDATE, "expected 0 or 1"));
                    }
                },
            },
            Some(_) => {
                return Err(EditorEnvError::new(
                    ENV_KIND,
                    "expected code-server or microsoft",
                ));
            }
        };
        Ok(Some(Self {
            kind,
            dir: path(ENV_DIR, dir)?,
            port,
            data: path(ENV_DATA, required(ENV_DATA)?)?,
            folder: path(ENV_FOLDER, required(ENV_FOLDER)?)?,
            workspace_file: get(ENV_WORKSPACE_FILE)
                .map(|file| path(ENV_WORKSPACE_FILE, file))
                .transpose()?,
            grace_secs,
            user: match get(ENV_USER).as_deref() {
                None => SessionUser::Root,
                Some(name) if name == SessionUser::Agent.name() => SessionUser::Agent,
                Some(name) if name == SessionUser::Root.name() => SessionUser::Root,
                Some(_) => return Err(EditorEnvError::new(ENV_USER, "expected agent or root")),
            },
        }))
    }
}

/// A variable of the editor server that can't be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorEnvError {
    /// The variable.
    pub var: &'static str,
    /// What is wrong.
    pub reason: String,
}

impl EditorEnvError {
    fn new(var: &'static str, reason: &str) -> Self {
        Self {
            var,
            reason: reason.to_owned(),
        }
    }
}

impl fmt::Display for EditorEnvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.var, self.reason)
    }
}

impl std::error::Error for EditorEnvError {}

#[cfg(test)]
mod tests;
