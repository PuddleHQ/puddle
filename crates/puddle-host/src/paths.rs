// SPDX-License-Identifier: AGPL-3.0-or-later
//! Where the host keeps its files, all under one data folder.

use std::path::{Path, PathBuf};

/// The files and folders of one Puddle installation, derived from its data folder.
///
/// The msb home (`msb/`) belongs to [`puddle_runtime::RuntimeLayout`]; everything else the host
/// writes is named here so one value says where a run's state lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostPaths {
    data: PathBuf,
}

impl HostPaths {
    /// Paths below `data` (an absolute folder, created on demand).
    #[must_use]
    pub fn new(data: impl Into<PathBuf>) -> Self {
        Self { data: data.into() }
    }

    /// The data folder.
    #[must_use]
    pub fn data(&self) -> &Path {
        &self.data
    }

    /// The SQLite database: rules, pending requests, audit.
    #[must_use]
    pub fn store(&self) -> PathBuf {
        self.data.join("puddle.db")
    }

    /// The settings documents: `global.json` and `sandboxes/<name>.json`.
    #[must_use]
    pub fn settings(&self) -> PathBuf {
        self.data.join("settings")
    }

    /// The workspace book: what Puddle knows about each workspace besides its volume.
    #[must_use]
    pub fn workspace_book(&self) -> PathBuf {
        self.data.join("workspaces.json")
    }

    /// Where an uploaded backup is read into before a restore is confirmed: one private folder per
    /// upload. Removed at the next start and after 24 hours.
    #[must_use]
    pub fn restore_staging(&self) -> PathBuf {
        self.data.join("restore-staging")
    }

    /// Where a confirmed restore waits for the next start: its marker file and the staged parts.
    #[must_use]
    pub fn restore_pending(&self) -> PathBuf {
        self.data.join("restore-pending")
    }

    /// The backups of the present state a restore makes before it changes anything, one folder per
    /// restore, named by the UTC time. Kept until the user removes one.
    #[must_use]
    pub fn safety_copies(&self) -> PathBuf {
        self.data.join("safety-copies")
    }

    /// What the last restore did, kept until the user has seen it.
    #[must_use]
    pub fn restore_result(&self) -> PathBuf {
        self.data.join("restore-result.json")
    }

    /// The workspaces whose disk the last quit could not trim, so the next start can say so.
    #[must_use]
    pub fn quit_trims(&self) -> PathBuf {
        self.data.join("quit-trims.json")
    }

    /// The proxies (addresses, never user names or passwords) that have had a company-proxy
    /// sign-in, kept outside the credential store so a store that comes back empty is noticed.
    #[must_use]
    pub fn proxy_sign_in_names(&self) -> PathBuf {
        self.data.join("proxy-sign-ins")
    }

    /// The markers that say which services have a login kept for a workspace (one small file for
    /// each), so a start with capture off need not ask the credential store.
    #[must_use]
    pub fn login_markers(&self) -> PathBuf {
        self.data.join("login-markers")
    }

    /// The root every sandbox mount source must be inside (msb mounts only from here).
    #[must_use]
    pub fn guest_share(&self) -> PathBuf {
        self.data.join("guest-share")
    }

    /// The browser front's secret-key halves, one file per workspace.
    #[must_use]
    pub fn front_keys(&self) -> PathBuf {
        self.data.join("front")
    }

    /// The port the browser front last bound, kept so its origin stays the same between runs.
    #[must_use]
    pub fn front_port_file(&self) -> PathBuf {
        self.front_keys().join("port")
    }

    /// The API's connection file (URL and token), for the CLI and `npm run dev`.
    #[must_use]
    pub fn connection_file(&self) -> PathBuf {
        self.data.join("api.json")
    }
}

#[cfg(test)]
mod tests;
