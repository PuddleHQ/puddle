// SPDX-License-Identifier: GPL-3.0-or-later
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
    walk(root, data, uid).writable
}

fn walk(root: &Path, data: &Path, uid: u32) -> Walk {
    walk_ancestors(&std::fs::canonicalize(root).unwrap(), data, uid).unwrap()
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
    assert_eq!(check(root.path(), &data, uid), []);
}

#[test]
fn a_folder_only_its_owner_can_enter_protects_everything_below_it() {
    let (root, data, uid) = tree(0o700, 0o775, 0o777);
    assert_eq!(check(root.path(), &data, uid), []);
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
    let walked = walk(root.path(), &later, uid);
    assert!(walked.dir.ends_with("a/b"), "{}", walked.dir.display());
    assert_eq!(names(root.path(), &walked.writable), ["a"]);
}

#[test]
fn a_symlinked_data_folder_is_judged_by_where_it_really_is() {
    let (root, data, uid) = tree(0o755, 0o775, 0o755);
    let link = root.path().join("link");
    std::os::unix::fs::symlink(&data, &link).unwrap();
    let found = check(root.path(), &link, uid);
    assert_eq!(names(root.path(), &found), ["a"]);
}

fn blocked_names(root: &Path, w: &Walk) -> Vec<(String, BlockedBy)> {
    let top = std::fs::canonicalize(root).unwrap();
    w.blocked
        .iter()
        .map(|b| {
            (
                b.path.strip_prefix(&top).unwrap().display().to_string(),
                b.by,
            )
        })
        .collect()
}

#[test]
fn a_file_where_a_folder_should_be_blocks_msb_and_is_named() {
    let (root, _data, uid) = tree(0o755, 0o755, 0o755);
    std::fs::write(root.path().join("a/file"), b"x").unwrap();
    mode(&root.path().join("a/file"), 0o666); // a file others can write is still no folder
    let w = walk(root.path(), &root.path().join("a/file/data"), uid);
    assert_eq!(
        blocked_names(root.path(), &w),
        [("a/file".to_string(), BlockedBy::NotAFolder)]
    );
    assert_eq!(w.writable, []);
}

#[test]
fn a_folder_owned_by_another_user_blocks_msb_even_below_a_private_folder() {
    // Seen as another user, everything the tree owns belongs to someone else, and the
    // owner-only top folder does not stop the check the way it stops the mode check.
    let (root, data, uid) = tree(0o700, 0o755, 0o755);
    let other = uid.wrapping_add(1);
    let w = walk(root.path(), &data, other);
    let by = BlockedBy::OtherOwner { uid };
    assert_eq!(
        blocked_names(root.path(), &w),
        [
            (String::new(), by),
            ("a".to_string(), by),
            ("a/b".to_string(), by),
            ("a/b/data".to_string(), by)
        ]
    );
}

#[test]
fn a_folder_owned_by_this_user_or_by_root_is_not_blocked() {
    let (root, data, uid) = tree(0o755, 0o755, 0o755);
    assert_eq!(walk(root.path(), &data, uid).blocked, []);
    // Root-owned folders pass for any user: `/` and `/usr` are the real ones.
    let w = walk_ancestors(Path::new("/"), Path::new("/usr"), uid.wrapping_add(1)).unwrap();
    assert_eq!(w.blocked, []);
}

#[test]
fn the_real_probe_reads_this_machine_and_says_nothing_without_a_folder() {
    assert_eq!(data_path(None), DataPathFacts::NoFolder);
    let dir = tempfile::tempdir().unwrap();
    assert!(matches!(
        data_path(Some(dir.path())),
        DataPathFacts::Checked { .. }
    ));
}

#[test]
fn a_folder_that_cannot_be_resolved_is_unreadable_and_named() {
    let facts = data_path(Some(Path::new("")));
    assert!(
        matches!(&facts, DataPathFacts::Unreadable { dir, detail }
                if dir == Path::new("") && !detail.is_empty()),
        "{facts:?}"
    );
}
