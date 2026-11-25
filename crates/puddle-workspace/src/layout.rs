// SPDX-License-Identifier: AGPL-3.0-or-later
//! Where a workspace lives in the guest: the volume's mount point, the checkouts below it and
//! Puddle's own directory beside them.
//!
//! ```text
//! /workspace/                 the volume root (ext4); never cloned into, it holds .puddle
//!   ├── lost+found/            only if mkfs made one (msb 0.7.7's doesn't); ignored
//!   ├── .puddle/               Puddle-owned (code-server data, a later ~/.vscode-server); the
//!   │   │                      delete check ignores it
//!   │   └── cloning/<repo>/    a clone in progress, moved to <repo>/ when git ends
//!   └── <repo>/                a git checkout (one or more), what the IDE opens
//! ```

use std::time::Duration;

use puddle_compute::ExecRequest;
use puddle_types::{CheckoutDir, GuestPath, SessionUser};

use crate::WorkspaceError;

/// Where a workspace's volume is mounted in its sandbox (ADR 0006). A sandbox holds one workspace,
/// so the folder is the same for every workspace; the id names the volume and the sandbox, never a
/// guest folder.
pub const WORKSPACE_DIR: &str = "/workspace";

/// Puddle's own directory on the volume, beside the checkouts.
pub const PUDDLE_DIR: &str = ".puddle";

/// The managed tools' folder below [`PUDDLE_DIR`].
const MISE_DIR: &str = "mise";

/// ext4's recovery directory; ignored at the volume root (msb 0.7.7's mkfs doesn't make one).
pub const LOST_AND_FOUND: &str = CheckoutDir::LOST_AND_FOUND;

/// How long a `sync` may take (a clone of a large repository leaves a lot to write).
pub(crate) const SYNC_TIMEOUT: Duration = Duration::from_secs(300);

/// The guest script that makes the volume ready for its session user (`guest/prepare.sh`).
pub const PREPARE_SH: &str = include_str!("../guest/prepare.sh");

/// The guest script that clones into a stage and moves the finished clone into place
/// (`guest/clone.sh`).
pub const CLONE_SH: &str = include_str!("../guest/clone.sh");

/// The folder below [`PUDDLE_DIR`] that clones are written in until git ends.
const CLONING_DIR: &str = "cloning";

/// How long the preparation may take. It is instant unless the volume changes owner, when every
/// file on it is given to the new one (once; a large `node_modules` is many files).
pub(crate) const PREPARE_TIMEOUT: Duration = Duration::from_mins(15);

/// A workspace's paths in the guest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    mount: GuestPath,
}

impl Layout {
    /// The layout of a workspace.
    ///
    /// # Errors
    ///
    /// Never in practice: [`WORKSPACE_DIR`] is a valid path. The error is
    /// [`WorkspaceError::Layout`].
    pub fn new() -> Result<Self, WorkspaceError> {
        let mount = guest(WORKSPACE_DIR)?;
        Ok(Self { mount })
    }

    /// The volume's mount point, `/workspace`.
    #[must_use]
    pub fn mount(&self) -> &GuestPath {
        &self.mount
    }

    /// Puddle's own directory, `/workspace/.puddle`.
    ///
    /// # Errors
    ///
    /// [`WorkspaceError::Layout`] (never for a valid layout).
    pub fn puddle_dir(&self) -> Result<GuestPath, WorkspaceError> {
        self.child(PUDDLE_DIR)
    }

    /// The managed tools' folder, `/workspace/.puddle/mise`: mise's binary, data, cache and
    /// state.
    ///
    /// # Errors
    ///
    /// [`WorkspaceError::Layout`] (never for a valid layout).
    pub fn mise_dir(&self) -> Result<GuestPath, WorkspaceError> {
        self.child(&format!("{PUDDLE_DIR}/{MISE_DIR}"))
    }

    /// The checkout directory `dir`, `/workspace/<dir>`.
    ///
    /// # Errors
    ///
    /// [`WorkspaceError::Layout`] (never for a valid layout: a [`CheckoutDir`] is one plain
    /// segment).
    pub fn checkout(&self, dir: &CheckoutDir) -> Result<GuestPath, WorkspaceError> {
        self.child(dir.as_str())
    }

    /// The command that creates Puddle's directory (owner-only) and gives the volume to `user`
    /// ([`PREPARE_SH`]), run as root after every boot of a sandbox with this workspace.
    /// Idempotent.
    #[must_use]
    pub fn prepare_request(&self, user: SessionUser) -> ExecRequest {
        ExecRequest::new(
            "sh",
            [
                "-c",
                PREPARE_SH,
                "puddle-prepare",
                self.mount.as_str(),
                user.name(),
            ]
            .map(str::to_owned),
        )
        .as_user("root")
        .with_timeout(PREPARE_TIMEOUT)
    }

    /// The clone of `url` into `/workspace/<dir>`: [`CLONE_SH`] with the URL, the stage
    /// `/workspace/.puddle/cloning/<dir>` and the checkout as its arguments. Git clones into
    /// the stage (`git clone -- <url> <stage>`: `--` keeps a URL that starts with `-` from being
    /// read as an option, and the URL stays one argument whatever it holds), and the finished
    /// clone is renamed into place, so a clone that dies part way never leaves a half-written
    /// checkout behind. Run it as the session user, with the proxy env.
    ///
    /// # Errors
    ///
    /// [`WorkspaceError::Layout`] (never for a valid layout).
    pub fn clone_request(
        &self,
        url: &str,
        dir: &CheckoutDir,
    ) -> Result<ExecRequest, WorkspaceError> {
        let checkout = self.checkout(dir)?;
        let stage = self.clone_stage(dir)?;
        Ok(ExecRequest::new(
            "sh",
            [
                "-c",
                CLONE_SH,
                "puddle-clone",
                url,
                stage.as_str(),
                checkout.as_str(),
            ]
            .map(str::to_owned),
        ))
    }

    /// Where the clone of checkout `dir` is written until git ends:
    /// `/workspace/.puddle/cloning/<dir>`.
    ///
    /// # Errors
    ///
    /// [`WorkspaceError::Layout`] (never for a valid layout).
    pub fn clone_stage(&self, dir: &CheckoutDir) -> Result<GuestPath, WorkspaceError> {
        self.child(&format!("{PUDDLE_DIR}/{CLONING_DIR}/{dir}"))
    }

    /// `sync`, as root: flushes everything the guest has cached to the volume. Directory entries
    /// aren't covered by `core.fsync=added` until the first fsync, so a VMM kill within
    /// seconds of a fresh clone or `git init` can lose the whole repository without it.
    #[must_use]
    pub fn sync_request() -> ExecRequest {
        ExecRequest::new("sync", Vec::<String>::new())
            .as_user("root")
            .with_timeout(SYNC_TIMEOUT)
    }

    fn child(&self, name: &str) -> Result<GuestPath, WorkspaceError> {
        guest(&format!("{}/{name}", self.mount))
    }
}

fn guest(path: &str) -> Result<GuestPath, WorkspaceError> {
    GuestPath::new(path).map_err(|e| WorkspaceError::Layout {
        reason: e.to_string(),
    })
}

#[cfg(test)]
mod tests;
