// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn the_private_key_text_is_built_without_growing_its_buffer() {
    let text = private_file(&[7u8; 32], &[9u8; 32]);
    assert!(text.len() <= TEXT_CAPACITY);
    assert_eq!(
        text.capacity(),
        TEXT_CAPACITY,
        "a growth would have left an unwiped copy"
    );
}

#[test]
fn a_new_pair_is_made_once_and_found_again() {
    let dir = tempfile::tempdir().unwrap();
    let first = LoginKey::open(dir.path()).unwrap();
    assert!(first.public_line().starts_with("ssh-ed25519 AAAA"));
    assert!(first.public_line().ends_with(&format!(" {COMMENT}")));
    assert!(first.private_path().is_file());
    let second = LoginKey::open(dir.path()).unwrap();
    assert_eq!(first, second);
}

#[test]
fn a_missing_private_half_or_a_damaged_public_half_makes_a_new_pair() {
    let dir = tempfile::tempdir().unwrap();
    let first = LoginKey::open(dir.path()).unwrap();
    std::fs::remove_file(first.private_path()).unwrap();
    let second = LoginKey::open(dir.path()).unwrap();
    assert_ne!(first.public_line(), second.public_line());
    std::fs::write(dir.path().join(PUBLIC_FILE), "not a key\n").unwrap();
    let third = LoginKey::open(dir.path()).unwrap();
    assert_ne!(second.public_line(), third.public_line());
    assert!(third.private_path().is_file());
}

#[test]
fn debug_never_shows_key_material() {
    let dir = tempfile::tempdir().unwrap();
    let key = LoginKey::open(dir.path()).unwrap();
    let shown = format!("{key:?}");
    let blob = key.public_line().split(' ').nth(1).unwrap();
    assert!(!shown.contains(blob), "{shown}");
}

#[cfg(unix)]
#[test]
fn the_private_file_is_owner_only_and_ssh_derives_the_public_line_from_it() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tempfile::tempdir().unwrap();
    let key = LoginKey::open(dir.path()).unwrap();
    let mode = std::fs::metadata(key.private_path())
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o077, 0, "mode {mode:o}");
    let out = std::process::Command::new("ssh-keygen")
        .args(["-y", "-f"])
        .arg(key.private_path())
        .output()
        .expect("ssh-keygen runs (OpenSSH client is a test requirement)");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let derived = String::from_utf8(out.stdout).unwrap();
    let mine: Vec<_> = key.public_line().split(' ').take(2).collect();
    let theirs: Vec<_> = derived.trim().split(' ').take(2).collect();
    assert_eq!(mine, theirs);
}

#[cfg(unix)]
#[test]
fn a_loosened_private_file_is_made_owner_only_and_the_same_key_is_kept() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tempfile::tempdir().unwrap();
    let first = LoginKey::open(dir.path()).unwrap();
    std::fs::set_permissions(first.private_path(), std::fs::Permissions::from_mode(0o644)).unwrap();
    let again = LoginKey::open(dir.path()).unwrap();
    assert_eq!(first, again);
    let mode = std::fs::metadata(again.private_path())
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600, "mode {mode:o}");
}

#[cfg(unix)]
#[test]
fn a_private_path_that_is_a_link_is_replaced_by_a_new_pair_and_the_link_target_is_left() {
    let dir = tempfile::tempdir().unwrap();
    let first = LoginKey::open(dir.path()).unwrap();
    let elsewhere = dir.path().join("elsewhere");
    let original = std::fs::read(first.private_path()).unwrap();
    std::fs::rename(first.private_path(), &elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, first.private_path()).unwrap();
    let second = LoginKey::open(dir.path()).unwrap();
    assert_ne!(first.public_line(), second.public_line());
    let meta = std::fs::symlink_metadata(second.private_path()).unwrap();
    assert!(meta.is_file() && !meta.is_symlink());
    assert_eq!(std::fs::read(&elsewhere).unwrap(), original);
}
