// SPDX-License-Identifier: AGPL-3.0-or-later
use std::io::Write as _;

use super::*;

#[test]
fn a_file_with_data_takes_at_least_its_data_and_a_missing_one_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("disk.raw");
    let mut file = std::fs::File::create(&path).unwrap();
    file.write_all(&vec![7_u8; 3 * 1024 * 1024]).unwrap();
    file.sync_all().unwrap();
    let used = allocated_bytes(&path).unwrap();
    assert!(used >= 3 * 1024 * 1024, "{used}");
    assert!(used < 4 * 1024 * 1024 + 65_536, "{used}");
    let error = allocated_bytes(&dir.path().join("missing")).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::NotFound);
}

/// Where the file system makes holes (every Linux one under test; not NTFS without the sparse
/// flag), a long file with a little data takes far less than its length.
#[cfg(unix)]
#[test]
fn a_sparse_file_takes_what_was_written_not_its_length() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("disk.raw");
    let mut file = std::fs::File::create(&path).unwrap();
    file.set_len(1024 * 1024 * 1024).unwrap();
    file.write_all(&[1_u8; 4096]).unwrap();
    file.sync_all().unwrap();
    let used = allocated_bytes(&path).unwrap();
    assert!(used >= 4096, "{used}");
    assert!(
        used < 64 * 1024 * 1024,
        "the file system made it dense: {used}"
    );
}

/// NTFS gives a file the whole of its length unless the file is marked sparse, and msb marks
/// not its length.
#[cfg(windows)]
#[test]
fn a_sparse_marked_ntfs_file_takes_what_was_written_not_its_length() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("disk.raw");
    std::fs::File::create(&path).unwrap();
    let marked = std::process::Command::new("fsutil")
        .args(["sparse", "setflag"])
        .arg(&path)
        .status()
        .unwrap();
    assert!(marked.success(), "fsutil could not mark the file sparse");
    let mut file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    file.set_len(1024 * 1024 * 1024).unwrap();
    file.write_all(&[1_u8; 4096]).unwrap();
    file.sync_all().unwrap();
    let used = allocated_bytes(&path).unwrap();
    assert!(used >= 4096, "{used}");
    assert!(
        used < 64 * 1024 * 1024,
        "apparent size reported as allocated: {used}"
    );
}
