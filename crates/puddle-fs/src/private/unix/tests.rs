// SPDX-License-Identifier: AGPL-3.0-or-later
use std::fs;

use super::*;

fn mode(p: &Path) -> u32 {
    fs::metadata(p).unwrap().permissions().mode() & 0o777
}

#[test]
fn new_folders_and_files_are_owner_only() {
    let dir = tempfile::tempdir().unwrap();
    let sub = dir.path().join("new").join("deeper");
    create_dir(&sub).unwrap();
    assert_eq!(mode(&sub), 0o700);
    let file = sub.join("f");
    drop(create_file(&file).unwrap());
    assert_eq!(mode(&file), 0o600);
}

#[test]
fn a_new_single_folder_is_exactly_0700_whatever_the_umask() {
    let dir = tempfile::tempdir().unwrap();
    let sub = dir.path().join("one");
    create_new_dir(&sub).unwrap();
    assert_eq!(
        fs::symlink_metadata(&sub).unwrap().permissions().mode() & 0o7777,
        0o700
    );
}

#[test]
fn a_folder_with_another_mode_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    expect_mode(dir.path(), mode(dir.path())).unwrap();
    let err = expect_mode(dir.path(), 0o751).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
    assert!(err.to_string().contains("expected mode 751"), "{err}");
}

#[test]
fn a_new_single_folder_is_refused_over_a_symlink() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target");
    fs::create_dir(&target).unwrap();
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    assert_eq!(
        create_new_dir(&link).unwrap_err().kind(),
        io::ErrorKind::AlreadyExists
    );
}

#[test]
fn an_existing_folder_keeps_its_mode() {
    let dir = tempfile::tempdir().unwrap();
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();
    create_dir(dir.path()).unwrap();
    assert_eq!(mode(dir.path()), 0o755);
}

#[test]
fn check_refuses_group_and_other_bits() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("f");
    fs::write(&file, b"x").unwrap();
    for (bits, ok) in [(0o600, true), (0o400, true), (0o640, false), (0o604, false)] {
        fs::set_permissions(&file, fs::Permissions::from_mode(bits)).unwrap();
        let got = check(&File::open(&file).unwrap());
        assert_eq!(got.is_ok(), ok, "{bits:o}");
        if !ok {
            assert!(
                matches!(got, Err(CheckError::Exposed(Exposed::Mode(m))) if m == bits),
                "{got:?}"
            );
        }
    }
}

#[test]
fn a_too_open_folder_is_tightened_and_reported() {
    let dir = tempfile::tempdir().unwrap();
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(tighten_dir(dir.path()).unwrap(), Some(Exposed::Mode(0o755)));
    assert_eq!(mode(dir.path()), 0o700);
    assert_eq!(tighten_dir(dir.path()).unwrap(), None);
}

#[test]
fn a_too_open_file_is_tightened_and_a_symlink_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("puddle.db");
    fs::write(&file, b"x").unwrap();
    fs::set_permissions(&file, fs::Permissions::from_mode(0o664)).unwrap();
    assert_eq!(tighten_file(&file).unwrap(), Some(Exposed::Mode(0o664)));
    assert_eq!(mode(&file), 0o600);
    assert_eq!(fs::read(&file).unwrap(), b"x");
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&file, &link).unwrap();
    assert_eq!(
        tighten_file(&link).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
}

#[test]
fn a_folder_that_cannot_be_read_is_an_error_not_a_silent_pass() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(
        tighten_dir(&dir.path().join("gone")).unwrap_err().kind(),
        io::ErrorKind::NotFound
    );
}
