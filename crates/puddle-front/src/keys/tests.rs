// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

fn name(n: &str) -> WorkspaceId {
    WorkspaceId::new(n).unwrap()
}

#[test]
fn a_half_is_made_once_kept_and_read_back_by_a_new_store() {
    let dir = tempfile::tempdir().unwrap();
    let first = FileHalves::new(dir.path().join("front"));
    let a = first.half(&name("a")).unwrap();
    assert_eq!(first.half(&name("a")).unwrap(), a);
    assert_ne!(first.half(&name("b")).unwrap(), a);
    let second = FileHalves::new(dir.path().join("front"));
    assert_eq!(second.half(&name("a")).unwrap(), a);
}

#[test]
fn forgetting_a_half_gives_the_name_a_new_one() {
    let dir = tempfile::tempdir().unwrap();
    let halves = FileHalves::new(dir.path());
    let old = halves.half(&name("a")).unwrap();
    halves.forget(&name("a")).unwrap();
    halves.forget(&name("a")).unwrap();
    assert_ne!(halves.half(&name("a")).unwrap(), old);
}

#[test]
fn a_half_is_there_from_its_first_use_until_it_is_forgotten() {
    let dir = tempfile::tempdir().unwrap();
    let halves = FileHalves::new(dir.path());
    assert!(!halves.has(&name("a")));
    halves.half(&name("a")).unwrap();
    assert!(halves.has(&name("a")));
    assert!(!halves.has(&name("b")));
    // A file another run left counts too.
    std::fs::write(dir.path().join("b.key"), "left").unwrap();
    assert!(halves.has(&name("b")));
    halves.forget(&name("a")).unwrap();
    assert!(!halves.has(&name("a")));
}

#[test]
fn a_file_that_is_not_a_half_is_replaced() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.key"), "not hex").unwrap();
    let halves = FileHalves::new(dir.path());
    let half = halves.half(&name("a")).unwrap();
    let again = FileHalves::new(dir.path()).half(&name("a")).unwrap();
    assert_eq!(half, again);
}

#[cfg(unix)]
#[test]
fn the_file_is_for_its_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    FileHalves::new(dir.path().join("front"))
        .half(&name("a"))
        .unwrap();
    let mode = std::fs::metadata(dir.path().join("front/a.key"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o077, 0, "{mode:o}");
}

#[test]
fn hex_round_trips_and_refuses_the_wrong_length() {
    let bytes: Vec<u8> = (0..32).collect();
    assert_eq!(unhex(&hex(&bytes)).unwrap().to_vec(), bytes);
    assert!(unhex("abcd").is_none());
    assert!(unhex(&"zz".repeat(32)).is_none());
    assert!(unhex(&"é".repeat(32)).is_none());
}
