// SPDX-License-Identifier: AGPL-3.0-or-later
//! [`Workspaces`]: create or reuse a workspace's volume, attach it to one sandbox at a time,
//! trim it, and delete it only after the user confirmed what would be lost.

use std::time::Duration;

use puddle_compute::{
    ComputeError, DiskSize, ExecRequest, FileMount, Runtime, Sandbox, SandboxSpec, VolumeInfo,
    VolumeMount, VolumeSpec,
};
use puddle_types::{
    CheckoutDir, GuestPath, ImageRef, MemoryMib, SandboxName, SessionUser, VolumeName, WorkspaceId,
    WorkspaceStatus, image_ref,
};
use tracing::{debug, info, warn};

use crate::check::{self, DeleteConfirmation, DeleteReport, Findings};
use crate::checkouts::{self, CheckoutList, FoundCheckout};
use crate::import::{Import, import_mount};
use crate::locks::{self, LockReport};
use crate::registry::{HoldKind, Holder, Registry};
use crate::trim::{self, TrimReport, tail};
use crate::{Layout, WorkspaceError};

/// Reconcile may remove leftovers of the maintenance sandboxes (see [`MAINTENANCE_PREFIX`]).
pub use puddle_types::MAINTENANCE_PREFIX;

/// Whether `name` is one of Puddle's maintenance sandboxes.
#[must_use]
pub fn is_maintenance_name(name: &str) -> bool {
    name.strip_prefix(MAINTENANCE_PREFIX)
        .is_some_and(|id| WorkspaceId::new(id).is_ok())
}

/// The maintenance sandbox for workspace `id`.
fn maintenance_name(id: &WorkspaceId) -> Result<SandboxName, WorkspaceError> {
    SandboxName::new(&format!("{MAINTENANCE_PREFIX}{id}")).map_err(|e| WorkspaceError::Layout {
        reason: e.to_string(),
    })
}

/// How long a `git clone` may take.
const CLONE_TIMEOUT: Duration = Duration::from_secs(600);

/// Settings of the workspace lifecycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceConfig {
    /// Size of a new workspace volume. Sparse on the host: it only takes what the guest writes
    /// (and keeps after a trim), plus the 64 MiB ext4 journal the runtime writes when it makes the
    /// volume. msb can't resize a volume, so this is the most a workspace can ever hold.
    pub default_size: DiskSize,
    /// Image of the short-lived sandbox that checks or trims a workspace no running sandbox
    /// has. Needs `sh`, `git`, `awk`, `fstrim`.
    pub maintenance_image: ImageRef,
    /// Its memory.
    pub maintenance_memory: MemoryMib,
}

impl WorkspaceConfig {
    /// Default [`WorkspaceConfig::default_size`]: 32 GiB.
    pub const DEFAULT_SIZE: DiskSize = DiskSize::gib(32);
    /// Default [`WorkspaceConfig::maintenance_image`]: the stock devcontainer image, pinned by digest.
    pub const DEFAULT_MAINTENANCE_IMAGE: &'static str = "mcr.microsoft.com/devcontainers/base:debian@sha256:41ff7ce12ec45f2704f27a6e091831186ad922ec8112e6339425bfefdf5a70f9";
    /// Default [`WorkspaceConfig::maintenance_memory`], in MiB.
    pub const DEFAULT_MAINTENANCE_MEMORY_MIB: u32 = 1024;
}

impl Default for WorkspaceConfig {
    fn default() -> Self {
        Self {
            default_size: Self::DEFAULT_SIZE,
            maintenance_image: image_ref!(Self::DEFAULT_MAINTENANCE_IMAGE),
            maintenance_memory: MemoryMib::new(Self::DEFAULT_MAINTENANCE_MEMORY_MIB)
                .unwrap_or(MemoryMib::DEFAULT),
        }
    }
}

/// What [`Workspaces::stop`] did.
#[derive(Debug)]
pub struct StopReport {
    /// One entry per workspace the sandbox owns: its trim, or why it failed. A failed trim
    /// doesn't stop the stop.
    pub trims: Vec<Result<TrimReport, WorkspaceError>>,
    /// The sandbox did not shut down in time and the runtime ended it by force: what the guest
    /// had not yet written to disk may be lost.
    pub forced: bool,
}

/// What [`Workspaces::delete`] removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Deleted {
    /// The workspace, whose volume is gone.
    pub workspace: WorkspaceId,
    /// The stopped sandbox that owned it, removed too.
    pub removed_sandbox: Option<SandboxName>,
}

/// The workspace lifecycle (ADR 0006): one named disk volume `ws-<id>` per workspace, mounted at
/// `/workspace` (see [`Layout`]), attached to one sandbox at a time.
///
/// Holder tracking is Puddle's own (point 8): a sandbox created with a workspace holds it until
/// [`Workspaces::sandbox_removed`], running or stopped, and any other attach is refused with
/// [`WorkspaceError::InUse`] naming it, before msb is asked. A running holder the registry doesn't
/// know (another Puddle, or one before a restart) is found through the runtime's own holder
/// lookup. The registry lives in memory; reconcile rebuilds it with [`Workspaces::adopt`].
///
/// Generic over the runtime: every method takes the [`Runtime`] it works on.
#[derive(Debug, Default)]
pub struct Workspaces {
    config: WorkspaceConfig,
    registry: Registry,
}

impl Workspaces {
    /// A lifecycle with `config` and nothing attached.
    #[must_use]
    pub fn new(config: WorkspaceConfig) -> Self {
        Self {
            config,
            registry: Registry::default(),
        }
    }

    /// Its settings.
    #[must_use]
    pub fn config(&self) -> &WorkspaceConfig {
        &self.config
    }

    /// Who holds workspace `id`, as far as Puddle tracks it.
    #[must_use]
    pub fn holder(&self, id: &WorkspaceId) -> Option<Holder> {
        self.registry.holder(id)
    }

    /// Records that `sandbox` (which exists already) was created with workspace `id`: for
    /// reconcile after a restart.
    pub fn adopt(&self, id: &WorkspaceId, sandbox: &SandboxName) {
        self.registry.attach(id, sandbox);
    }

    /// Releases every workspace `sandbox` held; call it after removing the sandbox.
    pub fn sandbox_removed(&self, sandbox: &SandboxName) {
        for id in self.registry.owned_by(sandbox) {
            self.registry.release(&id, sandbox);
        }
    }

    /// Creates Puddle's directory on the volume and gives the volume to `user` (see
    /// [`Layout::prepare_request`]); run after every boot. Idempotent. When the volume changed
    /// owner, the number of files given to `user` is logged.
    ///
    /// # Errors
    ///
    /// [`WorkspaceError::Runtime`]; [`WorkspaceError::Layout`] when the script fails or does not
    /// reach its end.
    pub async fn prepare_layout<S: Sandbox>(
        &self,
        sandbox: &S,
        id: &WorkspaceId,
        user: SessionUser,
    ) -> Result<(), WorkspaceError> {
        let request = Layout::new()?.prepare_request(user);
        let out = sandbox
            .exec(request)
            .await
            .map_err(|e| WorkspaceError::runtime("prepare the layout", id, e))?;
        let text = out.stdout_text();
        if !out.status.success() || out.stdout_truncated || text.lines().last() != Some("D") {
            return Err(WorkspaceError::Layout {
                reason: format!(
                    "preparing the volume for {user} failed (exit {}): {}",
                    out.status.code,
                    tail(&out.stderr_text())
                ),
            });
        }
        if let Some(count) = text.lines().find_map(|l| l.strip_prefix("R\t")) {
            info!(workspace = %id, %user, files = count, "the volume changed owner: every file on it given to the session user");
        }
        Ok(())
    }

    /// Clones `url` into the checkout directory `dir` of workspace `id`, as `user` (named, never
    /// left to the image's default user), with the sandbox's own environment (the proxy
    /// settings). Returns
    /// the checkout's path. Call [`Workspaces::sync`] after the last clone before calling it
    /// done: a VMM kill within seconds of a clone could otherwise lose the whole repository
    /// (directory entries aren't covered by `core.fsync=added` until the first fsync).
    ///
    /// # Errors
    ///
    /// [`WorkspaceError::Clone`] when `git clone` fails (git removes the folder it made);
    /// [`WorkspaceError::Layout`]; [`WorkspaceError::Runtime`].
    pub async fn clone_checkout<S: Sandbox>(
        &self,
        sandbox: &S,
        id: &WorkspaceId,
        url: &str,
        dir: &CheckoutDir,
        user: SessionUser,
    ) -> Result<GuestPath, WorkspaceError> {
        let layout = Layout::new()?;
        let checkout = layout.checkout(dir)?;
        let request = layout
            .clone_request(url, dir)?
            .with_timeout(CLONE_TIMEOUT)
            .as_user(user.name());
        let out = sandbox
            .exec(request)
            .await
            .map_err(|e| WorkspaceError::runtime("clone", id, e))?;
        if !out.status.success() {
            return Err(WorkspaceError::Clone {
                workspace: id.to_string(),
                reason: format!(
                    "git exited {}: {}",
                    out.status.code,
                    tail(&out.stderr_text())
                ),
            });
        }
        info!(workspace = %id, checkout = %dir, "checkout cloned");
        Ok(checkout)
    }

    /// Runs `sync` in `sandbox` (see [`Layout::sync_request`]); call it after anything that
    /// must survive a hard kill at once (a `git init`, a clone).
    ///
    /// # Errors
    ///
    /// [`WorkspaceError::Sync`]; [`WorkspaceError::Runtime`].
    pub async fn sync<S: Sandbox>(
        &self,
        sandbox: &S,
        id: &WorkspaceId,
    ) -> Result<(), WorkspaceError> {
        let out = sandbox
            .exec(Layout::sync_request())
            .await
            .map_err(|e| WorkspaceError::runtime("sync", id, e))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(WorkspaceError::Sync {
                workspace: id.to_string(),
                reason: format!(
                    "sync exited {}: {}",
                    out.status.code,
                    tail(&out.stderr_text())
                ),
            })
        }
    }

    /// Lists the checkouts directly under workspace `id`'s volume root (a directory, not a link,
    /// with a `.git` entry), at most [`crate::MAX_CHECKOUTS`], in `sandbox`, which must be
    /// running and have the volume mounted. Changes nothing in the guest.
    ///
    /// # Errors
    ///
    /// [`WorkspaceError::Checkouts`] when the listing exits non-zero or its output can't be
    /// read; [`WorkspaceError::Runtime`] when the exec fails, including a timeout.
    pub async fn list_checkouts<S: Sandbox>(
        &self,
        sandbox: &S,
        id: &WorkspaceId,
    ) -> Result<CheckoutList, WorkspaceError> {
        checkouts::run(sandbox, id, checkouts::Scope::All).await
    }

    /// Lists the names of the checkouts as [`Workspaces::list_checkouts`] finds them, without
    /// running git, so it works on an image that has none. Every checkout comes back with no
    /// remote, branch or commit and a clean tree: only the directories are known.
    ///
    /// # Errors
    ///
    /// As [`Workspaces::list_checkouts`], except [`WorkspaceError::GitMissing`].
    pub async fn list_checkout_names<S: Sandbox>(
        &self,
        sandbox: &S,
        id: &WorkspaceId,
    ) -> Result<CheckoutList, WorkspaceError> {
        checkouts::run(sandbox, id, checkouts::Scope::Names).await
    }

    /// Lists the checkouts on the volume of workspace `id` when no sandbox runs with it, in a
    /// short-lived maintenance sandbox that has the volume and no route out; it is removed again
    /// whatever the outcome. For a volume nothing lists, so the caller has no running workspace
    /// to ask. A volume a running sandbox holds, or a create or another maintenance run has, is
    /// [`WorkspaceError::InUse`] and is not touched.
    ///
    /// # Errors
    ///
    /// [`WorkspaceError::NotFound`] when the volume does not exist; [`WorkspaceError::InUse`];
    /// [`WorkspaceError::Checkouts`] when the listing fails; [`WorkspaceError::Runtime`].
    pub async fn list_checkouts_of_unheld_volume<R: Runtime>(
        &self,
        rt: &R,
        id: &WorkspaceId,
    ) -> Result<CheckoutList, WorkspaceError> {
        let info = self.volume(rt, id).await?;
        if let Some(holder) = self.running_holder(id, &info)? {
            return Err(WorkspaceError::InUse {
                workspace: id.to_string(),
                holder: holder.to_string(),
            });
        }
        match self.maintenance(rt, id, &info, Job::Checkouts).await? {
            JobOutput::Checkouts(list) => Ok(list),
            JobOutput::Check(_) | JobOutput::Trim(_) => unreachable_job(id),
        }
    }

    /// Reads the checkout `dir` of workspace `id` again: `None` when it is not a checkout (it is
    /// missing, a link, or has no `.git`).
    ///
    /// # Errors
    ///
    /// As [`Workspaces::list_checkouts`].
    pub async fn read_checkout<S: Sandbox>(
        &self,
        sandbox: &S,
        id: &WorkspaceId,
        dir: &CheckoutDir,
    ) -> Result<Option<FoundCheckout>, WorkspaceError> {
        Ok(checkouts::run(sandbox, id, checkouts::Scope::One(dir))
            .await?
            .checkouts
            .into_iter()
            .next())
    }

    /// Whether anything (a file, a directory, a link, even a dangling one) is at `dir` under
    /// workspace `id`'s volume root.
    ///
    /// # Errors
    ///
    /// [`WorkspaceError::Layout`]; [`WorkspaceError::Runtime`].
    pub async fn exists_on_volume<S: Sandbox>(
        &self,
        sandbox: &S,
        id: &WorkspaceId,
        dir: &CheckoutDir,
    ) -> Result<bool, WorkspaceError> {
        let path = Layout::new()?.checkout(dir)?;
        let request = ExecRequest::new(
            "sh",
            [
                "-c",
                "[ -e \"$1\" ] || [ -L \"$1\" ]",
                "puddle-exists",
                path.as_str(),
            ]
            .map(str::to_owned),
        )
        .as_user("root");
        let out = sandbox
            .exec(request)
            .await
            .map_err(|e| WorkspaceError::runtime("look for a folder", id, e))?;
        match out.status.code {
            0 => Ok(true),
            1 => Ok(false),
            // Not knowing is not "no": a clone must never go into a folder that may hold work.
            other => Err(WorkspaceError::Checkouts {
                workspace: id.to_string(),
                reason: format!(
                    "looking for {dir} exited {other}: {}",
                    tail(&out.stderr_text())
                ),
            }),
        }
    }

    /// Removes the stale git lock files a crash left in workspace `id`'s checkouts, so the user
    /// doesn't meet "Another git process seems to be running". Run it right after a boot (see
    /// [`Workspaces::after_boot`]): nothing removed while a `git` process runs in the guest
    /// ([`LockReport::skipped_busy`]).
    ///
    /// # Errors
    ///
    /// [`WorkspaceError::Locks`] when the script fails or its output can't be read;
    /// [`WorkspaceError::Runtime`].
    pub async fn clear_stale_locks<S: Sandbox>(
        &self,
        sandbox: &S,
        id: &WorkspaceId,
    ) -> Result<LockReport, WorkspaceError> {
        let report = locks::run(sandbox, id).await?;
        if report.skipped_busy {
            info!(workspace = %id, "git is running; stale lock files left alone");
        } else if report.removed_count() > 0 {
            info!(workspace = %id, removed = report.removed_count(), "stale git locks cleared");
        }
        for error in &report.errors {
            warn!(workspace = %id, %error, "a stale git lock could not be removed");
        }
        Ok(report)
    }

    /// What every boot of a sandbox with workspace `id` runs: [`Workspaces::prepare_layout`] for
    /// `user`,
    /// then [`Workspaces::clear_stale_locks`]. A lock failure is logged and doesn't fail the
    /// boot; it is in the report.
    ///
    /// # Errors
    ///
    /// The errors of [`Workspaces::prepare_layout`].
    pub async fn after_boot<S: Sandbox>(
        &self,
        sandbox: &S,
        id: &WorkspaceId,
        user: SessionUser,
    ) -> Result<Result<LockReport, WorkspaceError>, WorkspaceError> {
        self.prepare_layout(sandbox, id, user).await?;
        let locks = self.clear_stale_locks(sandbox, id).await;
        if let Err(e) = &locks {
            warn!(workspace = %id, error = %e, "clearing stale git locks failed");
        }
        Ok(locks)
    }

    /// Trims workspace `id` in `sandbox`, which must be running and have it mounted.
    ///
    /// # Errors
    ///
    /// [`WorkspaceError::Trim`]; [`WorkspaceError::Runtime`].
    pub async fn trim<S: Sandbox>(
        &self,
        sandbox: &S,
        id: &WorkspaceId,
    ) -> Result<TrimReport, WorkspaceError> {
        let report = trim::run(sandbox, id).await?;
        info!(workspace = %id, trimmed = ?report.trimmed_bytes, "workspace trimmed");
        Ok(report)
    }

    /// Stops `sandbox` the Puddle way: `fstrim` on every workspace it owns first (ADR 0006
    /// point 7), then [`Sandbox::stop`]. A failed trim is logged and reported, and the stop goes
    /// ahead. A sandbox that is already down is just stopped (a no-op).
    ///
    /// # Errors
    ///
    /// [`WorkspaceError::Runtime`] when the stop fails.
    pub async fn stop<S: Sandbox>(&self, sandbox: &S) -> Result<StopReport, WorkspaceError> {
        let name = sandbox.name();
        let mut trims = Vec::new();
        match sandbox.status().await {
            Ok(WorkspaceStatus::Running) => {
                for id in self.registry.owned_by(name) {
                    let result = self.trim(sandbox, &id).await;
                    if let Err(e) = &result {
                        warn!(sandbox = %name, workspace = %id, error = %e, "trim before stop failed");
                    }
                    trims.push(result);
                }
            }
            Ok(_) => {}
            // Not knowing whether it runs is not "down": the trim is skipped, and the report
            // says so for every workspace that was due one.
            Err(e) => {
                warn!(sandbox = %name, error = %e, "cannot tell whether the sandbox runs; no trim before the stop");
                for id in self.registry.owned_by(name) {
                    trims.push(Err(WorkspaceError::runtime(
                        "tell whether the sandbox runs before the trim",
                        &id,
                        e.clone(),
                    )));
                }
            }
        }
        sandbox
            .stop()
            .await
            .map_err(|e| WorkspaceError::runtime("stop the sandbox", name, e))?;
        Ok(StopReport {
            trims,
            forced: sandbox.stopped_by_force(),
        })
    }

    /// "Reclaim space" for workspace `id`: trims it in its running holder, or in a short-lived
    /// maintenance sandbox when no sandbox runs with it.
    ///
    /// # Errors
    ///
    /// [`WorkspaceError::NotFound`]; [`WorkspaceError::InUse`] while a create or another
    /// maintenance run has it; [`WorkspaceError::Trim`]; [`WorkspaceError::Runtime`].
    pub async fn reclaim_space<R: Runtime>(
        &self,
        rt: &R,
        id: &WorkspaceId,
    ) -> Result<TrimReport, WorkspaceError> {
        let info = self.volume(rt, id).await?;
        if let Some(holder) = self.running_holder(id, &info)? {
            let sandbox = connect_to_holder(rt, id, &holder).await?;
            return self.trim(&sandbox, id).await;
        }
        match self.maintenance(rt, id, &info, Job::Trim).await? {
            JobOutput::Trim(report) => Ok(report),
            JobOutput::Check(_) | JobOutput::Checkouts(_) => unreachable_job(id),
        }
    }

    /// The volume of `id`, or `None` when the runtime has none.
    async fn volume_if_any<R: Runtime>(
        &self,
        rt: &R,
        id: &WorkspaceId,
    ) -> Result<Option<VolumeInfo>, WorkspaceError> {
        rt.volume(&id.volume_name())
            .await
            .map_err(|e| WorkspaceError::runtime("look up the volume", id, e))
    }

    /// The volume of `id`, or [`WorkspaceError::NotFound`].
    async fn volume<R: Runtime>(
        &self,
        rt: &R,
        id: &WorkspaceId,
    ) -> Result<VolumeInfo, WorkspaceError> {
        rt.volume(&id.volume_name())
            .await
            .map_err(|e| WorkspaceError::runtime("look up the volume", id, e))?
            .ok_or_else(|| WorkspaceError::NotFound {
                workspace: id.to_string(),
            })
    }

    /// The running sandbox that has the volume (the runtime knows), or `None`. A create or a
    /// maintenance run in progress is [`WorkspaceError::InUse`].
    fn running_holder(
        &self,
        id: &WorkspaceId,
        info: &VolumeInfo,
    ) -> Result<Option<SandboxName>, WorkspaceError> {
        if let Some(h) = self
            .registry
            .holder(id)
            .filter(|h| h.kind != HoldKind::Attached)
        {
            return Err(WorkspaceError::InUse {
                workspace: id.to_string(),
                holder: h.sandbox.to_string(),
            });
        }
        match &info.holder {
            None => Ok(None),
            Some(name) => SandboxName::new(name).map(Some).map_err(|_| {
                // Not a name Puddle could have made: someone else's sandbox has it.
                WorkspaceError::InUse {
                    workspace: id.to_string(),
                    holder: name.clone(),
                }
            }),
        }
    }

    /// The sandbox that owns `id`, if it still exists.
    async fn existing_owner<R: Runtime>(
        &self,
        rt: &R,
        id: &WorkspaceId,
    ) -> Result<Option<SandboxName>, WorkspaceError> {
        let Some(owner) = self
            .registry
            .owner(id)
            .filter(|h| h.kind == HoldKind::Attached)
        else {
            return Ok(None);
        };
        let exists = rt
            .list()
            .await
            .map_err(|e| WorkspaceError::runtime("list sandboxes", id, e))?
            .iter()
            .any(|s| s.name == owner.sandbox.as_str());
        Ok(exists.then_some(owner.sandbox))
    }

    fn borrow(&self, id: &WorkspaceId, sandbox: &SandboxName) -> Result<(), WorkspaceError> {
        self.registry
            .borrow(id, sandbox)
            .map_err(|h| WorkspaceError::InUse {
                workspace: id.to_string(),
                holder: h.sandbox.to_string(),
            })
    }
}

/// The mount of `volume` at the workspace's mount point, always with kind disk and the size
/// (ADR 0006 point 9).
fn mount_for(volume: &VolumeName, info: &VolumeInfo) -> Result<VolumeMount, WorkspaceError> {
    let layout = Layout::new()?;
    let mount = VolumeMount::named(volume.clone(), layout.mount().clone());
    Ok(if info.size.as_mib() > 0 {
        mount.ensure_size(info.size)
    } else {
        mount
    })
}

/// The running sandbox `holder` that holds `id`'s volume. One that is booting, stopping or paused
/// still holds the volume but runs nothing: [`WorkspaceError::InUse`].
async fn connect_to_holder<R: Runtime>(
    rt: &R,
    id: &WorkspaceId,
    holder: &SandboxName,
) -> Result<R::Sandbox, WorkspaceError> {
    match rt.get(holder).await {
        Ok(sandbox) => Ok(sandbox),
        Err(ComputeError::InvalidState { .. }) => Err(WorkspaceError::InUse {
            workspace: id.to_string(),
            holder: holder.to_string(),
        }),
        Err(e) => Err(WorkspaceError::runtime("connect to the holder", id, e)),
    }
}

/// Removes a maintenance sandbox an earlier run left behind (down), and a stale directory with
/// its name. A running one is a maintenance run in progress elsewhere: [`WorkspaceError::InUse`].
async fn clear_leftover<R: Runtime>(
    rt: &R,
    id: &WorkspaceId,
    name: &SandboxName,
) -> Result<(), WorkspaceError> {
    let err = |op, e| WorkspaceError::runtime(op, id, e);
    let sandboxes = rt.list().await.map_err(|e| err("list sandboxes", e))?;
    if let Some(left) = sandboxes.iter().find(|s| s.name == name.as_str()) {
        if !left.status.is_down() {
            return Err(WorkspaceError::InUse {
                workspace: id.to_string(),
                holder: name.to_string(),
            });
        }
        rt.remove(name)
            .await
            .map_err(|e| err("remove a leftover maintenance sandbox", e))?;
    }
    if rt
        .stale_dirs()
        .await
        .map_err(|e| err("list stale directories", e))?
        .iter()
        .any(|s| s == name.as_str())
    {
        rt.remove_stale_dir(name)
            .await
            .map_err(|e| err("remove a stale directory", e))?;
    }
    Ok(())
}

/// Removes what a failed create of `name` left behind: a down sandbox record and a stale
/// directory, unless they were there before. Tries both; returns the first failure.
async fn cleanup_failed_create<R: Runtime>(
    rt: &R,
    name: &SandboxName,
    before: Before,
) -> Result<(), ComputeError> {
    let mut first = Ok(());
    if !before.record {
        match rt.list().await {
            Ok(list) => {
                if let Some(left) = list.iter().find(|s| s.name == name.as_str()) {
                    if left.status.is_down() {
                        debug!(sandbox = %name, "removing the record a failed create left");
                        first = first.and(rt.remove(name).await);
                    } else {
                        warn!(sandbox = %name, status = %left.status, "a failed create left a sandbox that is not down; leaving it");
                    }
                }
            }
            Err(e) => first = first.and(Err(e)),
        }
    }
    if !before.stale_dir {
        match rt.stale_dirs().await {
            Ok(dirs) if dirs.iter().any(|d| d == name.as_str()) => {
                debug!(sandbox = %name, "removing the directory a failed create left");
                first = first.and(rt.remove_stale_dir(name).await);
            }
            Ok(_) => {}
            Err(e) => first = first.and(Err(e)),
        }
    }
    first
}

fn unreachable_job<T>(id: &WorkspaceId) -> Result<T, WorkspaceError> {
    Err(WorkspaceError::Check {
        workspace: id.to_string(),
        reason: "internal error: the maintenance job returned the wrong output".into(),
    })
}

mod attachment;
mod delete;
mod import;
mod maintenance;

use attachment::Before;
pub use attachment::{Attachment, KeptSize};
pub use import::ImportReport;
use maintenance::{Job, JobOutput, Loan};

#[cfg(test)]
mod tests;
