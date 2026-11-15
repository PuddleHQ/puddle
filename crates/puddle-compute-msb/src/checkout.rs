// SPDX-License-Identifier: AGPL-3.0-or-later
//! The one mount source that is not under the guest share: the `.git` directory of a checkout the
//! user picked ([`puddle_compute::MountKind::Checkout`]).
//!
//! The rule is narrow on purpose. The source must be the canonical path of a real `.git`
//! directory, with a git directory's structure, outside every folder of Puddle's own and (on
//! Unix) owned by the user Puddle runs as. [`puddle_compute::SandboxSpec::validate`] already
//! keeps it out of any sandbox with a route, and [`crate::spec::builder`] passes it to msb
//! read-only like every other file mount, so msb's file server refuses every write whatever the
//! guest does to its own mount flags.
//!
//! What it does not cover, by design: the checks are by path, so a program of the same user that
//! swaps the folder between the check and msb's open (narrowed by a second check right before the
//! create) or puts a hard link or a bind mount inside `.git` gets past them. Such a program
//! already has the user's files; nothing of the checkout runs on the host either way.

#[cfg(unix)]
use std::fs::Metadata;
use std::path::Path;

use puddle_compute::ComputeError;

/// Checks `git_dir` against the checkout rule; `protected` are the folders no checkout may lie
/// in or hold (the msb home, the guest share, Puddle's data folder).
pub(crate) fn check(git_dir: &Path, protected: &[&Path]) -> Result<(), ComputeError> {
    #[cfg(unix)]
    let owner = Some(rustix::process::geteuid().as_raw());
    // On Windows the owner is not compared: a file that a member of Administrators makes is often
    // owned by that group, not by the user (git's own `safe.directory` check has to allow for it),
    // so the check would refuse ordinary checkouts. Nothing of the checkout runs on the host.
    #[cfg(not(unix))]
    let owner = None;
    check_as(git_dir, protected, owner)
}

/// [`check`] with the owner a checkout must have on Unix (`None`: not compared).
fn check_as(git_dir: &Path, protected: &[&Path], owner: Option<u32>) -> Result<(), ComputeError> {
    let refuse = |why: String| ComputeError::InvalidSpec {
        reason: format!("the checkout {} {why}", git_dir.display()),
    };
    if !git_dir.is_absolute() {
        return Err(refuse("is not an absolute path".into()));
    }
    if !on_local_drive(git_dir) {
        return Err(refuse("is on a network share, not a local drive".into()));
    }
    let canonical =
        dunce::canonicalize(git_dir).map_err(|e| refuse(format!("can't be resolved: {e}")))?;
    if canonical.as_os_str() != git_dir.as_os_str() {
        return Err(refuse(format!(
            "is not written as its canonical path {}: give it with no `..` and no link, junction \
             or other spelling of a folder in it",
            canonical.display()
        )));
    }
    puddle_compute::check_checkout_dir(git_dir).map_err(refuse)?;
    #[cfg(unix)]
    let meta =
        std::fs::symlink_metadata(git_dir).map_err(|e| refuse(format!("can't be read: {e}")))?;
    for root in protected {
        let root = dunce::canonicalize(root).map_err(|e| {
            refuse(format!(
                "can't be checked against Puddle's folder {}: {e}",
                root.display()
            ))
        })?;
        if within(&canonical, &root) {
            return Err(refuse(format!(
                "lies inside Puddle's folder {}",
                root.display()
            )));
        }
        if within(&root, &canonical) {
            return Err(refuse(format!("holds Puddle's folder {}", root.display())));
        }
    }
    match owner {
        #[cfg(unix)]
        Some(uid) => owned_by(&meta, uid).map_err(refuse),
        _ => Ok(()),
    }
}

/// Whether `path` starts on a local drive: always on Unix; on Windows a drive letter, never a
/// network share (`\\server\share`, also what a mapped network drive resolves to) or a device
/// path.
fn on_local_drive(path: &Path) -> bool {
    use std::path::{Component, Prefix};
    match path.components().next() {
        Some(Component::Prefix(p)) => matches!(p.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)),
        _ => true,
    }
}

/// Whether `path` is `root` or lies below it. On Windows and macOS, whose usual file systems
/// ignore case, the components are compared without case, so another spelling of a protected
/// folder is still that folder.
fn within(path: &Path, root: &Path) -> bool {
    within_as(path, root, cfg!(any(windows, target_os = "macos")))
}

/// [`within`], comparing without case when `fold`.
fn within_as(path: &Path, root: &Path, fold: bool) -> bool {
    if fold {
        let folded = |p: &Path| -> Vec<String> {
            p.components()
                .map(|c| c.as_os_str().to_string_lossy().to_lowercase())
                .collect()
        };
        folded(path).starts_with(&folded(root))
    } else {
        path.starts_with(root)
    }
}

/// On Unix the checkout must belong to the user Puddle runs as, as git itself asks of a
/// repository before it reads its config (`safe.directory`).
#[cfg(unix)]
fn owned_by(meta: &Metadata, uid: u32) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;
    if meta.uid() == uid {
        Ok(())
    } else {
        Err(format!(
            "belongs to another user (uid {}, Puddle runs as {uid})",
            meta.uid()
        ))
    }
}

#[cfg(test)]
mod tests;
