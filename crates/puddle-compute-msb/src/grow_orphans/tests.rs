// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

/// A sandbox folder with `root-disk.json` naming `layers` (paths relative to the folder).
fn sandbox(layers: &[&str], extra: &serde_json::Value) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let layers: Vec<_> = layers
        .iter()
        .map(|l| serde_json::json!({ "path": dir.path().join(l) }))
        .collect();
    let mut journal = serde_json::json!({ "layers": layers });
    if let (Some(map), Some(extra)) = (journal.as_object_mut(), extra.as_object()) {
        map.extend(extra.clone());
    }
    fs::write(dir.path().join(JOURNAL), journal.to_string()).unwrap();
    dir
}

fn file(dir: &Path, rel: &str) {
    let path = dir.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, rel).unwrap();
}

fn left(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn a_first_grow_leaves_the_stage_and_loses_the_old_head() {
    let dir = sandbox(&[".grow-aaa/upper.ext4"], &serde_json::json!({}));
    file(dir.path(), "upper.ext4");
    file(dir.path(), ".grow-aaa/upper.ext4");
    let removed = prune(dir.path()).unwrap();
    assert_eq!(removed, vec![dir.path().join("upper.ext4")]);
    assert_eq!(left(dir.path()), [".grow-aaa", JOURNAL]);
    assert!(dir.path().join(".grow-aaa/upper.ext4").is_file());
}

#[test]
fn a_second_grow_loses_the_first_stage_whole() {
    let dir = sandbox(&[".grow-bbb/upper.ext4"], &serde_json::json!({}));
    file(dir.path(), ".grow-aaa/upper.ext4");
    file(dir.path(), ".grow-aaa/base.raw");
    file(dir.path(), ".grow-bbb/upper.ext4");
    let removed = prune(dir.path()).unwrap();
    assert_eq!(removed, vec![dir.path().join(".grow-aaa")]);
    assert_eq!(left(dir.path()), [".grow-bbb", JOURNAL]);
}

#[test]
fn a_stage_that_holds_any_named_layer_stays_whole() {
    let dir = sandbox(
        &[".grow-aaa/base.raw", ".grow-aaa/upper.ext4"],
        &serde_json::json!({}),
    );
    file(dir.path(), ".grow-aaa/base.raw");
    file(dir.path(), ".grow-aaa/upper.ext4");
    file(dir.path(), ".grow-old/leftover");
    assert_eq!(
        prune(dir.path()).unwrap(),
        vec![dir.path().join(".grow-old")]
    );
    assert!(dir.path().join(".grow-aaa/base.raw").is_file());
}

#[test]
fn an_ungrown_root_keeps_its_head_and_a_flat_head_goes_like_a_managed_one() {
    let dir = sandbox(&["upper.ext4"], &serde_json::json!({}));
    file(dir.path(), "upper.ext4");
    file(dir.path(), "rootfs.raw");
    assert_eq!(
        prune(dir.path()).unwrap(),
        vec![dir.path().join("rootfs.raw")]
    );
    assert!(dir.path().join("upper.ext4").is_file());
    assert!(
        prune(dir.path()).unwrap().is_empty(),
        "a second run has nothing to do"
    );
}

#[test]
fn other_files_and_folders_are_never_touched() {
    let dir = sandbox(&[".grow-aaa/upper.ext4"], &serde_json::json!({}));
    file(dir.path(), ".grow-aaa/upper.ext4");
    file(dir.path(), "runtime/root-disk.json");
    file(dir.path(), "checkpoint-store/x");
    file(dir.path(), "logs/msb.log");
    file(dir.path(), ".compact-zzz/upper.ext4");
    file(dir.path(), ".grow-file");
    assert_eq!(prune(dir.path()).unwrap(), Vec::<PathBuf>::new());
    assert_eq!(
        left(dir.path()),
        [
            ".compact-zzz",
            ".grow-aaa",
            ".grow-file",
            "checkpoint-store",
            "logs",
            JOURNAL,
            "runtime"
        ]
    );
}

#[test]
fn nothing_is_deleted_while_the_journal_cannot_be_trusted() {
    // No journal.
    let dir = tempfile::tempdir().unwrap();
    file(dir.path(), "upper.ext4");
    assert_eq!(prune(dir.path()).unwrap(), Vec::<PathBuf>::new());
    // Unparsable journal, no layers, a pending growth, a named layer that is missing.
    let cases: [(&[u8], &str); 4] = [
        (b"{not json", "unparsable"),
        (br#"{"layers": []}"#, "empty"),
        (
            br#"{"layers": [], "growth_target": 1}"#,
            "pending and empty",
        ),
        (
            br#"{"layers": [{"path": "/nonexistent/x/upper.ext4"}]}"#,
            "missing",
        ),
    ];
    for (bytes, why) in cases {
        fs::write(dir.path().join(JOURNAL), bytes).unwrap();
        file(dir.path(), ".grow-aaa/upper.ext4");
        assert!(prune(dir.path()).unwrap().is_empty(), "{why}");
        assert!(dir.path().join("upper.ext4").is_file(), "{why}");
        assert!(dir.path().join(".grow-aaa").is_dir(), "{why}");
    }
}

#[test]
fn a_pending_growth_keeps_every_file() {
    let dir = sandbox(
        &[".grow-bbb/upper.ext4"],
        &serde_json::json!({ "growth_target": 17_179_869_184_u64 }),
    );
    file(dir.path(), "upper.ext4");
    file(dir.path(), ".grow-bbb/upper.ext4");
    assert_eq!(prune(dir.path()).unwrap(), Vec::<PathBuf>::new());
    assert!(dir.path().join("upper.ext4").is_file());
}

#[cfg(unix)]
#[test]
fn a_symlink_is_left_alone() {
    let dir = sandbox(&[".grow-aaa/upper.ext4"], &serde_json::json!({}));
    file(dir.path(), ".grow-aaa/upper.ext4");
    file(dir.path(), "elsewhere/data");
    std::os::unix::fs::symlink(dir.path().join("elsewhere"), dir.path().join(".grow-link"))
        .unwrap();
    std::os::unix::fs::symlink(
        dir.path().join("elsewhere/data"),
        dir.path().join("upper.ext4"),
    )
    .unwrap();
    // A link to nothing cannot be resolved and is left alone too.
    std::os::unix::fs::symlink(
        dir.path().join("missing"),
        dir.path().join(".grow-dangling"),
    )
    .unwrap();
    assert_eq!(prune(dir.path()).unwrap(), Vec::<PathBuf>::new());
    assert!(dir.path().join("elsewhere/data").is_file());
}

#[cfg(unix)]
#[test]
fn a_folder_that_cannot_be_emptied_is_an_error() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = sandbox(&[".grow-aaa/upper.ext4"], &serde_json::json!({}));
    file(dir.path(), ".grow-aaa/upper.ext4");
    file(dir.path(), ".grow-old/inner/x");
    let inner = dir.path().join(".grow-old/inner");
    fs::set_permissions(&inner, fs::Permissions::from_mode(0o500)).unwrap();
    let result = prune(dir.path());
    prune_logged("ws", dir.path()); // logs the failure and returns
    let _restored = fs::set_permissions(&inner, fs::Permissions::from_mode(0o700));
    // Root ignores the mode, so there the delete simply works.
    assert_eq!(result.is_err(), !rustix::process::geteuid().is_root());
    assert!(dir.path().join(".grow-aaa/upper.ext4").is_file());
}

#[test]
fn the_logged_form_deletes_the_same_files_and_never_fails() {
    let dir = sandbox(&[".grow-aaa/upper.ext4"], &serde_json::json!({}));
    file(dir.path(), "upper.ext4");
    file(dir.path(), ".grow-aaa/upper.ext4");
    prune_logged("ws", dir.path());
    assert_eq!(left(dir.path()), [".grow-aaa", JOURNAL]);
    // Nothing left to delete, and a folder that is not there: both return quietly.
    prune_logged("ws", dir.path());
    prune_logged("ws", &dir.path().join("gone"));
}
