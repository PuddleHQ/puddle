// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

fn id(s: &str) -> WorkspaceId {
    WorkspaceId::new(s).unwrap()
}

fn file() -> (tempfile::TempDir, QuitTrims) {
    let dir = tempfile::tempdir().unwrap();
    let file = QuitTrims::new(dir.path().join("quit-trims.json"));
    (dir, file)
}

#[test]
fn what_is_saved_is_loaded() {
    let (_dir, file) = file();
    let notes = BTreeMap::from([(id("acme"), "fstrim exited 1: no".to_owned())]);
    file.save(&notes);
    assert_eq!(file.load(), notes);
}

#[test]
fn saving_nothing_removes_the_file_and_no_file_loads_nothing() {
    let (dir, file) = file();
    assert!(file.load().is_empty());
    file.save(&BTreeMap::from([(id("acme"), "x".to_owned())]));
    assert!(dir.path().join("quit-trims.json").exists());
    file.save(&BTreeMap::new());
    assert!(!dir.path().join("quit-trims.json").exists());
    assert!(file.load().is_empty());
    file.save(&BTreeMap::new());
}

#[test]
fn a_save_that_fails_does_not_panic_and_leaves_what_is_there() {
    let (dir, file) = file();
    let path = dir.path().join("quit-trims.json");
    std::fs::create_dir(&path).unwrap();
    std::fs::write(path.join("keep"), "x").unwrap();
    file.save(&BTreeMap::from([(id("acme"), "x".to_owned())]));
    file.save(&BTreeMap::new());
    assert!(
        path.join("keep").exists(),
        "a folder where the file belongs"
    );
    assert!(file.load().is_empty());
}

#[test]
fn a_broken_or_foreign_file_is_ignored() {
    let (dir, file) = file();
    let path = dir.path().join("quit-trims.json");
    for body in [
        "not json".to_owned(),
        r#"{"version":2,"untrimmed":{"acme":"x"}}"#.to_owned(),
        r#"{"version":1}"#.to_owned(),
        r#"{"untrimmed":[]}"#.to_owned(),
    ] {
        std::fs::write(&path, body).unwrap();
        assert!(file.load().is_empty());
    }
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert!(file.load().is_empty(), "a folder where the file belongs");
}

#[test]
fn an_entry_that_is_no_workspace_id_is_dropped_and_the_rest_kept() {
    let (dir, file) = file();
    std::fs::write(
        dir.path().join("quit-trims.json"),
        r#"{"version":1,"untrimmed":{"acme":"x","Not An Id!":"y"}}"#,
    )
    .unwrap();
    assert_eq!(file.load(), BTreeMap::from([(id("acme"), "x".to_owned())]));
}

#[test]
fn the_problem_has_the_key_of_a_failed_stop_trim_and_names_the_quit() {
    let p = problem(&id("acme"), "Acme shop", "fstrim exited 1: no");
    assert_eq!(p.key, "trim:acme");
    assert_eq!(p.workspace, Some(id("acme")));
    assert_eq!(p.title, "Acme shop's disk was not trimmed when Puddle quit");
    assert!(p.detail.contains("fstrim exited 1"), "{}", p.detail);
    assert!(p.detail.contains("Reclaim space"), "{}", p.detail);
}
