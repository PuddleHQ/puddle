// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

fn set(ids: &[&str]) -> BTreeSet<String> {
    ids.iter().map(|id| (*id).to_owned()).collect()
}

#[test]
fn a_marker_exists_from_its_first_write_until_it_is_cleared() {
    let dir = tempfile::tempdir().unwrap();
    let marker = Marker::for_workspace(dir.path(), &WorkspaceId::new("acme").unwrap());
    assert_eq!(marker.path().unwrap().parent(), Some(dir.path()));
    assert!(!marker.exists());
    marker.write(&set(&["claude"]));
    assert!(marker.exists());
    marker.clear();
    assert!(!marker.exists());
    assert!(!Marker::none().exists());
    assert_eq!(Marker::none().path(), None);
}

#[test]
fn what_was_written_is_read_back_including_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let marker = Marker::in_file(dir.path().join("deeper").join("one.kept"));
    assert_eq!(marker.read(), None);
    marker.write(&set(&["claude", "github"]));
    assert_eq!(marker.read(), Some(set(&["claude", "github"])));
    marker.write(&set(&[]));
    assert_eq!(marker.read(), Some(set(&[])));
}

#[test]
fn a_file_that_is_not_ours_or_is_from_another_version_is_unknown() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("one.kept");
    let marker = Marker::in_file(path.clone());
    std::fs::write(&path, "claude\n").unwrap();
    assert_eq!(marker.read(), None);
    std::fs::write(&path, "puddle-logins-kept v9\nclaude\n").unwrap();
    assert_eq!(marker.read(), None);
    std::fs::write(&path, b"").unwrap();
    assert_eq!(marker.read(), None);
}

#[test]
fn a_service_keeps_the_time_it_was_first_noted_and_a_version_one_file_has_none() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("one.kept");
    let marker = Marker::in_file(path.clone());
    std::fs::write(&path, "puddle-logins-kept v1\nclaude\n").unwrap();
    assert_eq!(marker.read(), Some(set(&["claude"])));
    assert_eq!(marker.read_since().unwrap()["claude"], None);
    // Writing keeps what is already known and times only what is new since a known marker.
    marker.write(&set(&["claude", "github"]));
    let since = marker.read_since().unwrap();
    assert_eq!(since["claude"], None);
    assert!(since["github"].is_some());
    let first = since["github"];
    marker.write(&set(&["github"]));
    assert_eq!(marker.read_since().unwrap()["github"], first);
    // A marker that was not known cannot say when anything became kept.
    std::fs::remove_file(&path).unwrap();
    marker.write(&set(&["github"]));
    assert_eq!(marker.read_since().unwrap()["github"], None);
}

#[test]
fn a_time_that_is_not_a_number_reads_as_no_time() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("one.kept");
    std::fs::write(&path, "puddle-logins-kept v2\nclaude -5\ngithub 12 13\n").unwrap();
    let since = Marker::in_file(path).read_since().unwrap();
    assert_eq!(since["claude"], None);
    assert_eq!(since["github"], None);
}

#[test]
fn clearing_removes_the_file_and_a_file_that_cannot_be_removed_is_only_logged() {
    let dir = tempfile::tempdir().unwrap();
    let marker = Marker::in_file(dir.path().join("one.kept"));
    marker.clear();
    marker.write(&set(&["claude"]));
    marker.clear();
    assert_eq!(marker.read(), None);
    assert!(!dir.path().join("one.kept").exists());
    // A folder where the file should be cannot be removed as a file.
    let folder = dir.path().join("folder.kept");
    std::fs::create_dir(&folder).unwrap();
    Marker::in_file(folder.clone()).clear();
    assert!(folder.exists());
    Marker::none().clear();
}

#[test]
fn no_marker_never_knows_and_a_folder_that_cannot_be_made_only_costs_the_marker() {
    let none = Marker::none();
    none.write(&set(&["claude"]));
    assert_eq!(none.read(), None);
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("a-file");
    std::fs::write(&file, "x").unwrap();
    let blocked = Marker::in_file(file.join("below").join("one.kept"));
    blocked.write(&set(&["claude"]));
    assert_eq!(blocked.read(), None);
}
