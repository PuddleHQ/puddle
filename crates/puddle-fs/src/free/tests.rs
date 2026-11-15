// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn a_folder_that_exists_has_free_space_and_a_new_one_has_its_parents() {
    let dir = tempfile::tempdir().unwrap();
    let here = free_bytes(dir.path()).unwrap();
    assert!(here > 0, "a temp folder's drive has some room");
    let later = free_bytes(&dir.path().join("not").join("yet")).unwrap();
    // The same drive, so the two reads differ only by what else is writing: far less than 1 GiB.
    assert!(here.abs_diff(later) < 1 << 30, "{here} against {later}");
}

#[cfg(unix)]
#[test]
fn the_figure_is_the_blocks_available_to_a_user_times_the_block_size() {
    let dir = tempfile::tempdir().unwrap();
    let stat = rustix::fs::statvfs(dir.path()).unwrap();
    let free = free_bytes(dir.path()).unwrap();
    // Whole blocks only, and within what else writing meanwhile can change.
    assert_eq!(
        free % stat.f_frsize,
        0,
        "{free} is not whole blocks of {}",
        stat.f_frsize
    );
    let direct = stat.f_bavail.saturating_mul(stat.f_frsize);
    assert!(free.abs_diff(direct) < 1 << 30, "{free} against {direct}");
    assert!(free > 1, "{free}");
}

#[test]
fn a_path_with_no_existing_parent_is_not_found() {
    let error = free_bytes(Path::new("")).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::NotFound);
}
