// SPDX-License-Identifier: AGPL-3.0-or-later
//! Named volumes: who holds one, and the catalog view as [`VolumeInfo`].
//!
//! msb's own refusal of a second attach doesn't name the holder, so Puddle works it out:
//! the holder is the running sandbox whose configuration mounts the volume (ADR 0006 point 8).

use std::collections::BTreeMap;
use std::path::PathBuf;

use microsandbox::sandbox::{SandboxConfig, VolumeMount};
use microsandbox::volume::VolumeHandle;
use puddle_compute::{ComputeError, DiskSize, VolumeInfo};
use puddle_types::WorkspaceStatus;

/// One sandbox as the holder lookup needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Mounter {
    /// The sandbox.
    pub name: String,
    /// Its state.
    pub status: WorkspaceStatus,
    /// The named volumes its configuration mounts.
    pub volumes: Vec<String>,
}

/// The named volumes a sandbox configuration mounts.
pub(crate) fn named_volumes(config: &SandboxConfig) -> Vec<String> {
    config
        .spec
        .mounts
        .iter()
        .filter_map(|m| match m {
            VolumeMount::Named { name, .. } => Some(name.clone()),
            _ => None,
        })
        .collect()
}

/// The named volumes of the sandbox `name`, from its stored configuration. A configuration that
/// cannot be read is an error: reading it as "no volumes" would let a second writer attach a
/// volume that is in use.
pub(crate) fn mounted_volumes<E: std::fmt::Display>(
    name: &str,
    config: Result<SandboxConfig, E>,
) -> Result<Vec<String>, ComputeError> {
    match config {
        Ok(c) => Ok(named_volumes(&c)),
        Err(e) => Err(crate::error::runtime(
            "read the sandbox configuration",
            &format!(
                "the configuration stored for sandbox `{name}` in msb's database cannot be read ({e}), \
                 so its volumes are unknown and the single-writer check cannot run; remove the \
                 sandbox (its volumes are kept) and create it again"
            ),
        )),
    }
}

/// `name` as a [`Mounter`]. A sandbox that is down holds nothing, so an unreadable configuration
/// only matters (and fails) for one that is up or coming up.
pub(crate) fn mounter<E: std::fmt::Display>(
    name: &str,
    status: WorkspaceStatus,
    config: Result<SandboxConfig, E>,
) -> Result<Mounter, ComputeError> {
    let volumes = if status.is_down() {
        config.map_or_else(
            |e| {
                tracing::warn!(sandbox = name, error = %e, "the stored configuration of a stopped sandbox cannot be read; starting it will refuse");
                Vec::new()
            },
            |c| named_volumes(&c),
        )
    } else {
        mounted_volumes(name, config)?
    };
    Ok(Mounter {
        name: name.to_owned(),
        status,
        volumes,
    })
}

/// Volume name → the running sandbox that holds it. With two running holders (which msb
/// shouldn't allow), the first in name order is reported.
pub(crate) fn holders(mounters: &[Mounter]) -> BTreeMap<String, String> {
    let mut sorted: Vec<&Mounter> = mounters
        .iter()
        .filter(|m| m.status == WorkspaceStatus::Running)
        .collect();
    sorted.sort_by(|a, b| a.name.cmp(&b.name));
    let mut out = BTreeMap::new();
    for m in sorted {
        for v in &m.volumes {
            out.entry(v.clone()).or_insert_with(|| m.name.clone());
        }
    }
    out
}

/// Capacity in MiB from the catalog: the disk size, else the directory quota, else 0.
pub(crate) fn size_mib(capacity_bytes: Option<u64>, quota_mib: Option<u32>) -> DiskSize {
    let mib = capacity_bytes
        .map(|b| u32::try_from(b / (1024 * 1024)).unwrap_or(u32::MAX))
        .or(quota_mib)
        .unwrap_or(0);
    DiskSize::mib(mib)
}

/// Whole MiB in `bytes`, rounded up so a volume holding anything never reads as empty.
pub(crate) fn used_mib(bytes: u64) -> DiskSize {
    DiskSize::mib(u32::try_from(bytes.div_ceil(1024 * 1024)).unwrap_or(u32::MAX))
}

/// What the volume `name` takes on the host: the allocated part of its disk `image`. msb's own
/// catalog field for it is never updated after the volume is made, so the file is read. `None`
/// for a volume with no image of its own, or an image that cannot be looked at (logged, because
/// the workspace card then shows no use).
fn used_of(name: &str, image: Option<PathBuf>) -> Option<DiskSize> {
    let image = image?;
    match puddle_fs::allocated_bytes(&image) {
        Ok(bytes) => Some(used_mib(bytes)),
        Err(e) => {
            tracing::warn!(volume = name, image = %image.display(), error = %e, "the volume's use of the disk is unknown");
            None
        }
    }
}

/// The catalog entry as a [`VolumeInfo`].
pub(crate) fn info(handle: &VolumeHandle, holders: &BTreeMap<String, String>) -> VolumeInfo {
    VolumeInfo {
        name: handle.name().to_owned(),
        size: size_mib(handle.capacity_bytes(), handle.quota_mib()),
        used: used_of(handle.name(), handle.disk_path()),
        holder: holders.get(handle.name()).cloned(),
    }
}

#[cfg(test)]
mod tests;
