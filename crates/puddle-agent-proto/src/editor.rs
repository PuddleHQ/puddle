// SPDX-License-Identifier: GPL-3.0-or-later
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

/// How to run the editor server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorServer {
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
}

impl EditorServer {
    /// A server from [`GUEST_DIR`] on [`DEFAULT_PORT`], keeping its data in `data`, opening
    /// `folder`.
    #[must_use]
    pub fn new(data: impl Into<String>, folder: impl Into<String>, grace_secs: u32) -> Self {
        Self {
            dir: GUEST_DIR.to_owned(),
            port: DEFAULT_PORT,
            data: data.into(),
            folder: folder.into(),
            workspace_file: None,
            grace_secs,
        }
    }

    /// The same server, opening `file` instead of the folder when it holds a multi-root list.
    #[must_use]
    pub fn with_workspace_file(mut self, file: impl Into<String>) -> Self {
        self.workspace_file = Some(file.into());
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
        Ok(Some(Self {
            dir: path(ENV_DIR, dir)?,
            port,
            data: path(ENV_DATA, required(ENV_DATA)?)?,
            folder: path(ENV_FOLDER, required(ENV_FOLDER)?)?,
            workspace_file: get(ENV_WORKSPACE_FILE)
                .map(|file| path(ENV_WORKSPACE_FILE, file))
                .transpose()?,
            grace_secs,
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
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn read(vars: &[(&str, &str)]) -> Result<Option<EditorServer>, EditorEnvError> {
        let map: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        EditorServer::from_lookup(|k| map.get(k).cloned())
    }

    #[test]
    fn no_release_folder_means_no_server() {
        assert_eq!(read(&[]), Ok(None));
        assert_eq!(read(&[(ENV_PORT, "99999")]), Ok(None));
    }

    #[test]
    fn the_host_s_variables_read_back_and_defaults_apply() {
        let server = EditorServer::new("/workspaces/a/.puddle/code-server", "/workspaces/a", 600);
        let env = server.env();
        let vars: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
        assert_eq!(read(&vars), Ok(Some(server)));
        let minimal = read(&[(ENV_DIR, "/e"), (ENV_DATA, "/d"), (ENV_FOLDER, "/f")])
            .unwrap()
            .unwrap();
        assert_eq!((minimal.port, minimal.grace_secs), (DEFAULT_PORT, 300));
    }

    #[test]
    fn the_workspace_file_is_optional_and_reads_back() {
        let server = EditorServer::new("/d", "/f", 300).with_workspace_file("/f/w.code-workspace");
        let env = server.env();
        let vars: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
        assert_eq!(read(&vars), Ok(Some(server)));
        let without = read(&[(ENV_DIR, "/e"), (ENV_DATA, "/d"), (ENV_FOLDER, "/f")]);
        assert_eq!(without.unwrap().unwrap().workspace_file, None);
    }

    #[test]
    fn unusable_values_name_their_variable() {
        let ok = [(ENV_DIR, "/e"), (ENV_DATA, "/d"), (ENV_FOLDER, "/f")];
        let with = |extra: (&'static str, &'static str)| {
            let mut v = ok.to_vec();
            v.retain(|(k, _)| *k != extra.0);
            v.push(extra);
            read(&v).unwrap_err().to_string()
        };
        assert!(with((ENV_PORT, "80")).starts_with(ENV_PORT));
        assert!(with((ENV_PORT, "x")).starts_with(ENV_PORT));
        assert!(with((ENV_GRACE, "29")).starts_with(ENV_GRACE));
        assert!(with((ENV_GRACE, "86401")).starts_with(ENV_GRACE));
        assert!(with((ENV_DATA, "relative")).starts_with(ENV_DATA));
        assert!(with((ENV_FOLDER, "/a/../b")).starts_with(ENV_FOLDER));
        assert!(with((ENV_WORKSPACE_FILE, "w.code-workspace")).starts_with(ENV_WORKSPACE_FILE));
        assert!(with((ENV_DIR, "e")).starts_with(ENV_DIR));
        for missing in [ENV_DATA, ENV_FOLDER] {
            let mut v = ok.to_vec();
            v.retain(|(k, _)| *k != missing);
            assert!(read(&v).unwrap_err().to_string().starts_with(missing));
        }
        assert_eq!(
            read(&[
                (ENV_DIR, "/e"),
                (ENV_DATA, "/d"),
                (ENV_FOLDER, "/f"),
                (ENV_GRACE, "30")
            ])
            .unwrap()
            .unwrap()
            .grace_secs,
            30
        );
    }
}
