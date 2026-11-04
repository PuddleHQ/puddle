// SPDX-License-Identifier: GPL-3.0-or-later
//! The folders above Puddle's data folder, read by msb's own rule (`verify_ancestors` in msb's
//! catalog storage): walking from `/` down, a folder that group or everyone can write in, without
//! the sticky bit, lets another account swap the folders below it, so msb refuses to open its
//! catalog under it. The walk stops caring once it passes a folder only its owner can enter
//! (mode `0o700`, owned by this user): nobody else can reach what is below it.
//!
//! msb also refuses an ancestor that is not a folder or that another user (not root) owns; those
//! are not reported here, only the permission case users hit by accident.

use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use crate::facts::{DataPathFacts, WritableFolder};

/// Group or world write.
const WRITE_BY_OTHERS: u32 = 0o022;
const STICKY: u32 = 0o1000;
/// Any group or world access.
const ANY_BY_OTHERS: u32 = 0o077;

/// The facts about the folders above `dir` (`None`: Puddle's data folder is unknown).
pub(crate) fn data_path(dir: Option<&Path>) -> Option<DataPathFacts> {
    let Some(dir) = dir else {
        return Some(DataPathFacts::NoFolder);
    };
    let euid = rustix::process::geteuid().as_raw();
    Some(match writable_ancestors(Path::new("/"), dir, euid) {
        Ok((dir, writable)) => DataPathFacts::Checked { dir, writable },
        Err(e) => DataPathFacts::Unreadable {
            dir: dir.to_path_buf(),
            detail: e.to_string(),
        },
    })
}

/// The folders msb would refuse among those from `top` down to `dir` (or its nearest existing
/// parent, which Puddle creates below), as seen by the user `euid`. Returns the folder that was
/// walked and the offenders, top first.
fn writable_ancestors(
    top: &Path,
    dir: &Path,
    euid: u32,
) -> io::Result<(PathBuf, Vec<WritableFolder>)> {
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
    for folder in chain {
        let meta = std::fs::symlink_metadata(folder)?;
        let mode = meta.mode();
        if !private_boundary && mode & WRITE_BY_OTHERS != 0 && mode & STICKY == 0 {
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
    Ok((resolved, found))
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn mode(path: &Path, bits: u32) {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(bits)).unwrap();
    }

    /// A tree `top/a/b/data` with the given modes for `top`, `a` and `b`, and the user's uid.
    fn tree(top: u32, a: u32, b: u32) -> (tempfile::TempDir, PathBuf, u32) {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("a/b/data");
        std::fs::create_dir_all(&data).unwrap();
        mode(&data, 0o700);
        mode(&root.path().join("a/b"), b);
        mode(&root.path().join("a"), a);
        mode(root.path(), top);
        let uid = std::fs::metadata(root.path()).unwrap().uid();
        (root, data, uid)
    }

    fn names(root: &Path, found: &[WritableFolder]) -> Vec<String> {
        found
            .iter()
            .map(|f| {
                f.path
                    .strip_prefix(std::fs::canonicalize(root).unwrap())
                    .unwrap()
                    .display()
                    .to_string()
            })
            .collect()
    }

    fn check(root: &Path, data: &Path, uid: u32) -> Vec<WritableFolder> {
        // `top` is the tree's root, so the real folders above it (the temp folder) do not count.
        writable_ancestors(&std::fs::canonicalize(root).unwrap(), data, uid)
            .unwrap()
            .1
    }

    #[test]
    fn a_group_writable_folder_below_a_group_readable_home_is_named_with_its_mode() {
        // Ubuntu: home 0750 (here `top`), ~/.local 0775 (here `a`), ~/.local/share 0755.
        let (root, data, uid) = tree(0o750, 0o775, 0o755);
        let found = check(root.path(), &data, uid);
        assert_eq!(names(root.path(), &found), ["a"]);
        assert_eq!(found[0].mode, 0o775);
        assert!(found[0].owned);
        assert!(found[0].group_writable());
        assert!(!found[0].world_writable());
    }

    #[test]
    fn a_world_writable_folder_is_named_and_flagged_as_world_writable() {
        let (root, data, uid) = tree(0o755, 0o755, 0o757);
        let found = check(root.path(), &data, uid);
        assert_eq!(names(root.path(), &found), ["a/b"]);
        assert!(found[0].world_writable());
        assert!(!found[0].group_writable());
    }

    #[test]
    fn every_offending_folder_is_named_top_first() {
        let (root, data, uid) = tree(0o775, 0o775, 0o755);
        let found = check(root.path(), &data, uid);
        assert_eq!(names(root.path(), &found), ["", "a"]);
    }

    #[test]
    fn the_sticky_bit_makes_a_shared_folder_safe() {
        let (root, data, uid) = tree(0o1777, 0o3770, 0o755);
        assert!(check(root.path(), &data, uid).is_empty());
    }

    #[test]
    fn a_folder_only_its_owner_can_enter_protects_everything_below_it() {
        let (root, data, uid) = tree(0o700, 0o775, 0o777);
        assert!(check(root.path(), &data, uid).is_empty());
    }

    #[test]
    fn a_private_folder_of_another_user_protects_nothing() {
        // Seen as a different user, the 0700 top is not "ours" and so is no boundary.
        let (root, data, uid) = tree(0o700, 0o775, 0o755);
        let found = check(root.path(), &data, uid.wrapping_add(1));
        assert_eq!(names(root.path(), &found), ["a"]);
        assert!(!found[0].owned);
    }

    #[test]
    fn a_group_readable_but_group_closed_folder_is_no_boundary() {
        // 0750 and 0705 both leave someone with access, so a writable folder below is flagged.
        let (root, data, uid) = tree(0o705, 0o775, 0o755);
        assert_eq!(check(root.path(), &data, uid).len(), 1);
        let (root, data, uid) = tree(0o750, 0o775, 0o755);
        assert_eq!(check(root.path(), &data, uid).len(), 1);
    }

    #[test]
    fn the_data_folder_itself_counts_once_it_is_group_writable() {
        let (root, data, uid) = tree(0o755, 0o755, 0o755);
        mode(&data, 0o770);
        let found = check(root.path(), &data, uid);
        assert_eq!(names(root.path(), &found), ["a/b/data"]);
    }

    #[test]
    fn a_data_folder_that_does_not_exist_yet_is_judged_by_its_nearest_parent() {
        let (root, _data, uid) = tree(0o755, 0o775, 0o755);
        let later = root.path().join("a/b/not/yet");
        let (walked, found) =
            writable_ancestors(&std::fs::canonicalize(root.path()).unwrap(), &later, uid).unwrap();
        assert!(walked.ends_with("a/b"), "{}", walked.display());
        assert_eq!(names(root.path(), &found), ["a"]);
    }

    #[test]
    fn a_symlinked_data_folder_is_judged_by_where_it_really_is() {
        let (root, data, uid) = tree(0o755, 0o775, 0o755);
        let link = root.path().join("link");
        std::os::unix::fs::symlink(&data, &link).unwrap();
        let found = check(root.path(), &link, uid);
        assert_eq!(names(root.path(), &found), ["a"]);
    }

    #[test]
    fn the_real_probe_reads_this_machine_and_says_nothing_without_a_folder() {
        assert_eq!(data_path(None), Some(DataPathFacts::NoFolder));
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            data_path(Some(dir.path())),
            Some(DataPathFacts::Checked { .. })
        ));
    }
}
