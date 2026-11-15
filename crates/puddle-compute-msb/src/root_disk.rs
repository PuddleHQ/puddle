// SPDX-License-Identifier: AGPL-3.0-or-later
//! How full a sandbox's root disk is, read from msb's root-disk journal.
//!
//! msb keeps `root-disk.json` in the sandbox's own folder, out of the guest's reach (the copy in
//! `runtime/` is only a guard the guest cannot use), so the files it names are the host's to read.

use std::fs;
use std::future::Future;
use std::path::{Path, PathBuf};

use puddle_compute::{ComputeError, DiskSize, RootDiskGrowth, RootDiskUse};
use serde::Deserialize;

/// The journal's file name in the sandbox's folder (msb's name for it).
const JOURNAL: &str = "root-disk.json";

/// The part of msb's journal that names the disk files; its other fields are not needed here.
#[derive(Deserialize)]
struct Journal {
    /// The disk files, oldest first, the last one the writable head.
    layers: Vec<Layer>,
}

#[derive(Deserialize)]
struct Layer {
    path: PathBuf,
}

/// How full the root disk of the sandbox whose folder is `sandbox_dir` is: its capacity and what the files that make up the disk take on the host.
/// `None` when there is no disk to look at (not made yet, a layout this does not know, or a
/// sandbox of the older layout, whose journal msb has not yet moved out of `runtime/`).
pub(crate) fn usage(sandbox_dir: &Path) -> Option<RootDiskUse> {
    let files = disk_files(sandbox_dir)?;
    let size = capacity(files.last()?)?;
    let mut used: u64 = 0;
    for file in &files {
        used = used.saturating_add(puddle_fs::allocated_bytes(file).ok()?);
    }
    let size = to_mib(size);
    // A qcow2 head can take more than the base's length; the disk is then as full as it can be.
    let used = to_mib(used.div_ceil(MIB) * MIB).min(size);
    Some(RootDiskUse {
        size: DiskSize::mib(size),
        used: DiskSize::mib(used),
    })
}

/// What msb's plan for a root-disk size change says.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum GrowPlan {
    /// msb refuses the change; the text says why (a size that is not above the current one).
    Refused(String),
    /// The running guest gets the room now.
    Live,
    /// The room arrives at the next start (a stopped sandbox, or a runtime that cannot grow a
    /// running disk).
    Deferred,
}

/// Reads msb's dry-run `plan` of a root-disk size change, as this platform can carry it out.
pub(crate) fn grow_plan(plan: &microsandbox::sandbox::SandboxModificationPlan) -> GrowPlan {
    on_platform(read_plan(plan), LIVE_GROW_WORKS)
}

/// What msb's dry-run `plan` says, whatever the platform.
fn read_plan(plan: &microsandbox::sandbox::SandboxModificationPlan) -> GrowPlan {
    use microsandbox::{ModificationDisposition, PlannedChange};
    if let Some(conflict) = plan.conflicts.first() {
        return GrowPlan::Refused(conflict.message.clone());
    }
    let live = plan.changes.iter().any(|change| {
        matches!(
            change,
            PlannedChange::Config(c)
                if c.field == "root_disk_size" && c.disposition == ModificationDisposition::Live
        )
    });
    if live {
        GrowPlan::Live
    } else {
        GrowPlan::Deferred
    }
}

/// Whether the runtime's hypervisor can take a live grow. msb plans one on Windows too, but the
/// pinned libkrun fork keeps the raw disk handle at its old length there, so the guest's online
/// ext4 expansion fails with an I/O error and leaves the block device grown and the filesystem
/// not (seen in vm-windows CI). Live grow on Windows waits for the fork fix
/// (`upstream-issues/libkrun-windows-live-grow/`); until then the disk grows at a restart.
pub(crate) const LIVE_GROW_WORKS: bool = !cfg!(windows);

/// `plan` as this platform can carry it out: a live plan becomes a deferred one where
/// `live_works` is false, so the host restarts the workspace instead of growing it under the guest.
pub(crate) fn on_platform(plan: GrowPlan, live_works: bool) -> GrowPlan {
    match plan {
        GrowPlan::Live if !live_works => GrowPlan::Deferred,
        other => other,
    }
}

/// The three msb calls of a root-disk grow, so the choice between them is tested without a real
/// msb database.
pub(crate) trait GrowSdk {
    /// msb's dry run of the change.
    fn plan(&self) -> impl Future<Output = Result<GrowPlan, ComputeError>> + Send;
    /// The change on a running guest.
    fn apply_live(&self) -> impl Future<Output = Result<(), ComputeError>> + Send;
    /// The change at the next start.
    fn apply_next_start(&self) -> impl Future<Output = Result<(), ComputeError>> + Send;
}

/// Grows through `sdk`: a refused plan is an invalid spec, a live plan applies now, any other at
/// the next start.
pub(crate) async fn grow(sdk: &impl GrowSdk) -> Result<RootDiskGrowth, ComputeError> {
    match sdk.plan().await? {
        GrowPlan::Refused(reason) => Err(ComputeError::InvalidSpec { reason }),
        GrowPlan::Live => sdk.apply_live().await.map(|()| RootDiskGrowth::Live),
        GrowPlan::Deferred => sdk
            .apply_next_start()
            .await
            .map(|()| RootDiskGrowth::NextStart),
    }
}

const MIB: u64 = 1024 * 1024;

/// qcow2 files start with this magic, then the version and the backing-file fields; the virtual
/// size is the big-endian `u64` at byte 24.
const QCOW2_MAGIC: [u8; 4] = *b"QFI\xfb";

/// What the disk file `head` can hold: a qcow2 file's virtual size, else (a raw disk) its length.
fn capacity(head: &Path) -> Option<u64> {
    use std::io::Read as _;
    let mut file = fs::File::open(head).ok()?;
    let length = file.metadata().ok()?.len();
    let mut header = [0_u8; 32];
    if file.read_exact(&mut header).is_ok() && header[..4] == QCOW2_MAGIC {
        let size: [u8; 8] = header[24..32].try_into().ok()?;
        return Some(u64::from_be_bytes(size));
    }
    Some(length)
}

fn to_mib(bytes: u64) -> u32 {
    u32::try_from(bytes / MIB).unwrap_or(u32::MAX)
}

/// The files of the root disk, oldest first: the journal's layers when msb keeps one, else the
/// plain `upper.ext4` of a managed root disk.
fn disk_files(sandbox_dir: &Path) -> Option<Vec<PathBuf>> {
    let journal = sandbox_dir.join(JOURNAL);
    if journal.exists() {
        let bytes = fs::read(&journal).ok()?;
        let parsed: Journal = serde_json::from_slice(&bytes).ok()?;
        let files: Vec<PathBuf> = parsed.layers.into_iter().map(|l| l.path).collect();
        return (!files.is_empty()).then_some(files);
    }
    let upper = sandbox_dir.join("upper.ext4");
    upper.is_file().then(|| vec![upper])
}

#[cfg(test)]
mod tests;
