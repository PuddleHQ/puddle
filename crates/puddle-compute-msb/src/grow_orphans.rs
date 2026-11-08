// SPDX-License-Identifier: GPL-3.0-or-later
//! Deletes the root-disk images that a grow of a stopped sandbox leaves behind.
//!
//! msb grows a stopped root by copying the head into a new `.grow-XXXXXX` folder and pointing
//! `root-disk.json` at the copy; it never deletes the image it replaced, so every grow of a
//! stopped workspace keeps one more full copy of the used size on the host. Nothing reads the
//! replaced files again: the next start takes the journal's layers, not the configured path.
//! Whether deleting them is safe was checked with a real stopped sandbox (grow, delete, start,
//! `e2fsck`, repeat).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// msb's journal in the sandbox folder.
const JOURNAL: &str = "root-disk.json";

/// The names of a root image msb creates directly in the sandbox folder before it ever grows.
const HEAD_NAMES: [&str; 2] = ["upper.ext4", "rootfs.raw"];

/// The prefix of the folder msb stages a stopped grow in.
const STAGE_PREFIX: &str = ".grow-";

/// The part of the journal that says which files make up the disk.
#[derive(Deserialize)]
struct Journal {
    layers: Vec<Layer>,
    /// Set while a live grow has not finished; msb completes it on the next start.
    #[serde(default)]
    growth_target: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct Layer {
    path: PathBuf,
}

/// Deletes what `root-disk.json` in `sandbox_dir` no longer names: the pre-grow head
/// (`upper.ext4` or `rootfs.raw`) and every `.grow-*` folder that holds none of the journal's
/// layers. Call it only while the sandbox is stopped.
///
/// Returns the deleted paths. Deletes nothing (and returns an empty list) when the folder has no
/// journal, the journal does not parse, it names no layers, a growth is pending, or a layer it
/// names is missing: in every such state the journal may not be the one that won, so the files
/// beside it may be the only copy.
pub(crate) fn prune(sandbox_dir: &Path) -> io::Result<Vec<PathBuf>> {
    let Ok(bytes) = fs::read(sandbox_dir.join(JOURNAL)) else {
        return Ok(Vec::new());
    };
    let Ok(journal) = serde_json::from_slice::<Journal>(&bytes) else {
        return Ok(Vec::new());
    };
    if journal.growth_target.is_some() || journal.layers.is_empty() {
        return Ok(Vec::new());
    }
    let mut named = Vec::with_capacity(journal.layers.len());
    for layer in &journal.layers {
        let Ok(path) = dunce::canonicalize(&layer.path) else {
            return Ok(Vec::new());
        };
        named.push(path);
    }
    let mut removed = Vec::new();
    for entry in fs::read_dir(sandbox_dir)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        // A path that cannot be resolved is left alone, as is a symlink (`file_type` does not follow it).
        let Ok(real) = dunce::canonicalize(&path) else {
            continue;
        };
        if file_type.is_file() && HEAD_NAMES.contains(&name.as_ref()) {
            if !named.contains(&real) {
                fs::remove_file(&path)?;
                removed.push(path);
            }
        } else if file_type.is_dir()
            && name.starts_with(STAGE_PREFIX)
            && !named.iter().any(|n| n.starts_with(&real))
        {
            fs::remove_dir_all(&path)?;
            removed.push(path);
        }
    }
    removed.sort();
    Ok(removed)
}

/// [`prune`] for the sandbox `name`, with its outcome in the log; never fails (an orphan that stays
/// only costs disk).
pub(crate) fn prune_logged(name: &str, sandbox_dir: &Path) {
    match prune(sandbox_dir) {
        Ok(removed) if removed.is_empty() => {}
        Ok(removed) => tracing::info!(sandbox = name, removed = ?removed,
            "deleted the root images a stopped grow left behind"),
        Err(e) => tracing::warn!(sandbox = name, error = %e,
            "could not delete the root images a stopped grow left behind"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A sandbox folder with `root-disk.json` naming `layers` (paths relative to the folder).
    fn sandbox(layers: &[&str], extra: serde_json::Value) -> tempfile::TempDir {
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
        let dir = sandbox(&[".grow-aaa/upper.ext4"], serde_json::json!({}));
        file(dir.path(), "upper.ext4");
        file(dir.path(), ".grow-aaa/upper.ext4");
        let removed = prune(dir.path()).unwrap();
        assert_eq!(removed, vec![dir.path().join("upper.ext4")]);
        assert_eq!(left(dir.path()), [".grow-aaa", JOURNAL]);
        assert!(dir.path().join(".grow-aaa/upper.ext4").is_file());
    }

    #[test]
    fn a_second_grow_loses_the_first_stage_whole() {
        let dir = sandbox(&[".grow-bbb/upper.ext4"], serde_json::json!({}));
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
            serde_json::json!({}),
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
        let dir = sandbox(&["upper.ext4"], serde_json::json!({}));
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
        let dir = sandbox(&[".grow-aaa/upper.ext4"], serde_json::json!({}));
        file(dir.path(), ".grow-aaa/upper.ext4");
        file(dir.path(), "runtime/root-disk.json");
        file(dir.path(), "checkpoint-store/x");
        file(dir.path(), "logs/msb.log");
        file(dir.path(), ".compact-zzz/upper.ext4");
        file(dir.path(), ".grow-file");
        assert!(prune(dir.path()).unwrap().is_empty());
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
        assert!(prune(dir.path()).unwrap().is_empty());
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
            serde_json::json!({ "growth_target": 17_179_869_184_u64 }),
        );
        file(dir.path(), "upper.ext4");
        file(dir.path(), ".grow-bbb/upper.ext4");
        assert!(prune(dir.path()).unwrap().is_empty());
        assert!(dir.path().join("upper.ext4").is_file());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_is_left_alone() {
        let dir = sandbox(&[".grow-aaa/upper.ext4"], serde_json::json!({}));
        file(dir.path(), ".grow-aaa/upper.ext4");
        file(dir.path(), "elsewhere/data");
        std::os::unix::fs::symlink(dir.path().join("elsewhere"), dir.path().join(".grow-link"))
            .unwrap();
        std::os::unix::fs::symlink(
            dir.path().join("elsewhere/data"),
            dir.path().join("upper.ext4"),
        )
        .unwrap();
        assert!(prune(dir.path()).unwrap().is_empty());
        assert!(dir.path().join("elsewhere/data").is_file());
    }

    #[cfg(unix)]
    #[test]
    fn a_folder_that_cannot_be_emptied_is_an_error() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = sandbox(&[".grow-aaa/upper.ext4"], serde_json::json!({}));
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
        let dir = sandbox(&[".grow-aaa/upper.ext4"], serde_json::json!({}));
        file(dir.path(), "upper.ext4");
        file(dir.path(), ".grow-aaa/upper.ext4");
        prune_logged("ws", dir.path());
        assert_eq!(left(dir.path()), [".grow-aaa", JOURNAL]);
        // Nothing left to delete, and a folder that is not there: both return quietly.
        prune_logged("ws", dir.path());
        prune_logged("ws", &dir.path().join("gone"));
    }
}
