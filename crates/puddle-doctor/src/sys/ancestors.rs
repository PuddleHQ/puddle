// SPDX-License-Identifier: GPL-3.0-or-later
//! The folders above Puddle's data folder, read by msb's own rule (`verify_ancestors` in msb's
//! catalog storage): walking from `/` down, a folder that group or everyone can write in, without
//! the sticky bit, lets another account swap the folders below it, so msb refuses to open its
//! catalog under it. The walk stops caring once it passes a folder only its owner can enter
//! (mode `0o700`, owned by this user): nobody else can reach what is below it.
//!
//! msb also refuses, at every level and even below that owner-only folder, an ancestor that is not
//! a folder or that a user other than this one and root owns; both are reported too. The macOS
//! build of msb adds an ACL check that this Linux-only probe does not make.

use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use crate::facts::{BlockedBy, BlockedFolder, DataPathFacts, WritableFolder};

/// Group or world write.
const WRITE_BY_OTHERS: u32 = 0o022;
const STICKY: u32 = 0o1000;
/// Any group or world access.
const ANY_BY_OTHERS: u32 = 0o077;

/// The facts about the folders above `dir` (`None`: Puddle's data folder is unknown).
pub(crate) fn data_path(dir: Option<&Path>) -> DataPathFacts {
    let Some(dir) = dir else {
        return DataPathFacts::NoFolder;
    };
    let euid = rustix::process::geteuid().as_raw();
    match walk_ancestors(Path::new("/"), dir, euid) {
        Ok(walk) => DataPathFacts::Checked {
            dir: walk.dir,
            writable: walk.writable,
            blocked: walk.blocked,
        },
        Err(e) => DataPathFacts::Unreadable {
            dir: dir.to_path_buf(),
            detail: e.to_string(),
        },
    }
}

/// What the walk found.
struct Walk {
    dir: PathBuf,
    writable: Vec<WritableFolder>,
    blocked: Vec<BlockedFolder>,
}

/// The folders msb would refuse among those from `top` down to `dir` (or its nearest existing
/// parent, which Puddle creates below), as seen by the user `euid`, top first.
fn walk_ancestors(top: &Path, dir: &Path, euid: u32) -> io::Result<Walk> {
    let absolute = std::path::absolute(dir)?;
    let existing = absolute
        .ancestors()
        .find(|p| p.exists())
        .unwrap_or(Path::new("/"));
    let resolved = std::fs::canonicalize(existing)?;
    let mut chain: Vec<&Path> = resolved
        .ancestors()
        .take_while(|p| p.starts_with(top))
        .collect();
    chain.reverse();
    let mut private_boundary = false;
    let mut found = Vec::new();
    let mut blocked = Vec::new();
    for folder in chain {
        let meta = std::fs::symlink_metadata(folder)?;
        let mode = meta.mode();
        if !meta.is_dir() {
            blocked.push(BlockedFolder {
                path: folder.to_path_buf(),
                by: BlockedBy::NotAFolder,
            });
        } else if meta.uid() != euid && meta.uid() != 0 {
            blocked.push(BlockedFolder {
                path: folder.to_path_buf(),
                by: BlockedBy::OtherOwner { uid: meta.uid() },
            });
        }
        if meta.is_dir() && !private_boundary && mode & WRITE_BY_OTHERS != 0 && mode & STICKY == 0 {
            found.push(WritableFolder {
                path: folder.to_path_buf(),
                mode: mode & 0o7777,
                owned: meta.uid() == euid,
            });
        }
        if !private_boundary {
            private_boundary = meta.uid() == euid && mode & ANY_BY_OTHERS == 0;
        }
    }
    Ok(Walk {
        dir: resolved,
        writable: found,
        blocked,
    })
}

#[cfg(test)]
mod tests;
