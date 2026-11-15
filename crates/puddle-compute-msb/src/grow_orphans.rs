// SPDX-License-Identifier: AGPL-3.0-or-later
//! Deletes the root-disk images that a grow of a stopped sandbox leaves behind.
//!
//! msb grows a stopped root by copying the head into a new `.grow-XXXXXX` folder and pointing
//! `root-disk.json` at the copy; it never deletes the image it replaced, so every grow of a
//! stopped workspace keeps one more full copy of the used size on the host. Nothing reads the
//! replaced files again: the next start takes the journal's layers, not the configured path.
//! Whether deleting them is safe was checked with a real stopped sandbox (grow, delete, start,
//! `e2fsck`, repeat).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// msb's journal in the sandbox folder.
const JOURNAL: &str = "root-disk.json";

/// The names of a root image msb creates directly in the sandbox folder before it ever grows.
const HEAD_NAMES: [&str; 2] = ["upper.ext4", "rootfs.raw"];

/// The prefix of the folder msb stages a stopped grow in.
const STAGE_PREFIX: &str = ".grow-";

/// The part of the journal that says which files make up the disk.
#[derive(Deserialize)]
struct Journal {
    layers: Vec<Layer>,
    /// Set while a live grow has not finished; msb completes it on the next start.
    #[serde(default)]
    growth_target: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct Layer {
    path: PathBuf,
}

/// Deletes what `root-disk.json` in `sandbox_dir` no longer names: the pre-grow head
/// (`upper.ext4` or `rootfs.raw`) and every `.grow-*` folder that holds none of the journal's
/// layers. Call it only while the sandbox is stopped.
///
/// Returns the deleted paths. Deletes nothing (and returns an empty list) when the folder has no
/// journal, the journal does not parse, it names no layers, a growth is pending, or a layer it
/// names is missing: in every such state the journal may not be the one that won, so the files
/// beside it may be the only copy.
pub(crate) fn prune(sandbox_dir: &Path) -> io::Result<Vec<PathBuf>> {
    let Ok(bytes) = fs::read(sandbox_dir.join(JOURNAL)) else {
        return Ok(Vec::new());
    };
    let Ok(journal) = serde_json::from_slice::<Journal>(&bytes) else {
        return Ok(Vec::new());
    };
    if journal.growth_target.is_some() || journal.layers.is_empty() {
        return Ok(Vec::new());
    }
    let mut named = Vec::with_capacity(journal.layers.len());
    for layer in &journal.layers {
        let Ok(path) = dunce::canonicalize(&layer.path) else {
            return Ok(Vec::new());
        };
        named.push(path);
    }
    let mut removed = Vec::new();
    for entry in fs::read_dir(sandbox_dir)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        // A path that cannot be resolved is left alone, as is a symlink (`file_type` does not follow it).
        let Ok(real) = dunce::canonicalize(&path) else {
            continue;
        };
        if file_type.is_file() && HEAD_NAMES.contains(&name.as_ref()) {
            if !named.contains(&real) {
                fs::remove_file(&path)?;
                removed.push(path);
            }
        } else if file_type.is_dir()
            && name.starts_with(STAGE_PREFIX)
            && !named.iter().any(|n| n.starts_with(&real))
        {
            fs::remove_dir_all(&path)?;
            removed.push(path);
        }
    }
    removed.sort();
    Ok(removed)
}

/// [`prune`] for the sandbox `name`, with its outcome in the log; never fails (an orphan that stays
/// only costs disk).
pub(crate) fn prune_logged(name: &str, sandbox_dir: &Path) {
    match prune(sandbox_dir) {
        Ok(removed) if removed.is_empty() => {}
        Ok(removed) => tracing::info!(sandbox = name, removed = ?removed,
            "deleted the root images a stopped grow left behind"),
        Err(e) => tracing::warn!(sandbox = name, error = %e,
            "could not delete the root images a stopped grow left behind"),
    }
}

#[cfg(test)]
mod tests;
