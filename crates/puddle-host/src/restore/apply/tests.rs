// SPDX-License-Identifier: AGPL-3.0-or-later
use std::sync::Arc;

use puddle_store::{Limits, Store, SystemClock};

use super::*;
use crate::restore::stage_archive;

fn sql(paths: &HostPaths, statements: &str) {
    rusqlite::Connection::open(paths.store())
        .unwrap()
        .execute_batch(statements)
        .unwrap();
}

fn rule(host: &str) -> String {
    format!(
        "INSERT INTO rules (scope, pattern_kind, pattern, effect, created_at, created_by, user_id) \
         VALUES ('global', 'exact', '{host}', 'allow', 1, 'cli', 1);"
    )
}

fn data_folder(root: &Path, name: &str, hosts: &[&str], book: &str, global: &str) -> HostPaths {
    let paths = HostPaths::new(root.join(name));
    fs::create_dir_all(paths.settings()).unwrap();
    drop(Store::open(&paths.store(), Arc::new(SystemClock), Limits::default()).unwrap());
    sql(&paths, &hosts.iter().map(|h| rule(h)).collect::<String>());
    fs::write(paths.workspace_book(), book).unwrap();
    fs::write(paths.settings().join("global.json"), global).unwrap();
    paths
}

const BOOK_HERE: &str = r#"{"version":4,"workspaces":[{"id":"w-here","name":"Here","owner":1}]}"#;
const BOOK_THERE: &str = r#"{"version":4,"workspaces":[{"id":"w-there","name":"There","owner":1}],"retired_ids":["w-old"]}"#;

/// A machine with its own data and a staged backup of another, ready to confirm.
struct Setup {
    root: tempfile::TempDir,
    here: HostPaths,
    staging_id: String,
}

fn setup() -> Setup {
    let root = tempfile::tempdir().unwrap();
    let there = data_folder(
        root.path(),
        "there",
        &["a.example.org", "new.example.org"],
        BOOK_THERE,
        r#"{"version":1,"a":1,"b":2}"#,
    );
    let here = data_folder(
        root.path(),
        "here",
        &["a.example.org", "mine.example.org"],
        BOOK_HERE,
        r#"{"version":1,"a":9,"c":3}"#,
    );
    let mut archive = crate::backup_archive(&there).unwrap();
    let mut bytes = Vec::new();
    io::Read::read_to_end(&mut archive.file, &mut bytes).unwrap();
    let staged = stage_archive(&bytes[..], &here.restore_staging()).unwrap();
    Setup {
        root,
        here,
        staging_id: staged.id,
    }
}

fn rules(paths: &HostPaths) -> Vec<String> {
    let conn = rusqlite::Connection::open(paths.store()).unwrap();
    let mut stmt = conn
        .prepare("SELECT pattern FROM rules ORDER BY pattern")
        .unwrap();
    stmt.query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn read(path: PathBuf) -> serde_json::Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn ids(book: &serde_json::Value) -> Vec<String> {
    book["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["id"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn confirming_makes_the_safety_copy_and_changes_no_live_file() {
    let s = setup();
    let before = rules(&s.here);
    let requested = request(&s.here, &s.staging_id, ApplyMode::Add, "summary").unwrap();
    let copy = s.here.safety_copies().join(&requested.safety_copy);
    assert!(copy.join("puddle.db").is_file());
    assert!(copy.join("manifest.json").is_file());
    // The safety copy is an ordinary backup of the present state.
    let in_copy = rusqlite::Connection::open(copy.join("puddle.db")).unwrap();
    let n: i64 = in_copy
        .query_row("SELECT count(*) FROM rules", [], |r| r.get(0))
        .unwrap();
    assert_eq!(usize::try_from(n).unwrap(), before.len());
    assert_eq!(rules(&s.here), before);
    assert!(s.here.restore_pending().join("marker.json").is_file());
    assert!(
        s.here
            .restore_pending()
            .join("staged")
            .join("puddle.db")
            .is_file()
    );
    assert!(!s.here.restore_staging().join(&s.staging_id).exists());
}

#[test]
fn a_restore_that_cannot_write_its_safety_copy_does_not_start() {
    let s = setup();
    // A file where the safety copies' folder should be.
    fs::write(s.here.safety_copies(), b"in the way").unwrap();
    let err = request(&s.here, &s.staging_id, ApplyMode::Replace, "").unwrap_err();
    assert!(matches!(err, RequestError::Failed(_)), "{err}");
    assert!(err.to_string().contains("safety copy"), "{err}");
    assert!(!s.here.restore_pending().join("marker.json").exists());
    assert!(
        s.here.restore_staging().join(&s.staging_id).is_dir(),
        "the staged copy stays, so the user can retry"
    );
}

#[test]
fn an_unknown_staging_id_and_a_second_confirm_are_refused() {
    let s = setup();
    for bad in ["../x", &"0".repeat(32)] {
        let err = request(&s.here, bad, ApplyMode::Add, "").unwrap_err();
        assert!(matches!(err, RequestError::NotStaged), "{err}");
    }
    request(&s.here, &s.staging_id, ApplyMode::Add, "").unwrap();
    // A second upload, confirmed while the first waits.
    let again = request(&s.here, &s.staging_id, ApplyMode::Add, "").unwrap_err();
    assert!(
        matches!(
            again,
            RequestError::NotStaged | RequestError::AlreadyPending
        ),
        "{again}"
    );
}

#[test]
fn add_at_the_next_start_merges_and_keeps_the_marker_for_last() {
    let s = setup();
    let requested = request(&s.here, &s.staging_id, ApplyMode::Add, "rules: 1 added.").unwrap();
    let outcome = apply_pending(&s.here).unwrap();
    assert!(outcome.applied, "{}", outcome.message);
    assert_eq!(outcome.safety_copy, requested.safety_copy);
    assert!(
        outcome.message.contains("rules: 1 added."),
        "{}",
        outcome.message
    );
    assert_eq!(
        rules(&s.here),
        ["a.example.org", "mine.example.org", "new.example.org"]
    );
    // A setting set here wins; one only the backup has is added.
    let global = read(s.here.settings().join("global.json"));
    assert_eq!(
        (
            global["a"].as_i64(),
            global["b"].as_i64(),
            global["c"].as_i64()
        ),
        (Some(9), Some(2), Some(3))
    );
    let book = read(s.here.workspace_book());
    assert_eq!(ids(&book), ["w-here", "w-there"]);
    assert_eq!(book["retired_ids"][0], "w-old");
    // Nothing is left pending, and the result is kept for the user.
    assert!(!s.here.restore_pending().exists());
    let result: RestoreOutcome =
        serde_json::from_slice(&fs::read(s.here.restore_result()).unwrap()).unwrap();
    assert_eq!(result, outcome);
    // The store opens on the result.
    drop(Store::open(&s.here.store(), Arc::new(SystemClock), Limits::default()).unwrap());
    assert!(s.here.safety_copies().join(&requested.safety_copy).is_dir());
}

#[test]
fn replace_takes_the_backups_rules_settings_and_workspaces() {
    let s = setup();
    request(&s.here, &s.staging_id, ApplyMode::Replace, "").unwrap();
    let outcome = apply_pending(&s.here).unwrap();
    assert!(outcome.applied, "{}", outcome.message);
    assert_eq!(rules(&s.here), ["a.example.org", "new.example.org"]);
    let global = read(s.here.settings().join("global.json"));
    assert_eq!(
        (global["a"].as_i64(), global["c"].as_i64()),
        (Some(1), None)
    );
    assert_eq!(ids(&read(s.here.workspace_book())), ["w-there"]);
}

#[test]
fn no_stale_wal_survives_beside_the_new_database() {
    let s = setup();
    request(&s.here, &s.staging_id, ApplyMode::Replace, "").unwrap();
    // A WAL with a committed row in it, as a run that did not shut down cleanly leaves.
    let conn = rusqlite::Connection::open(s.here.store()).unwrap();
    conn.pragma_update(None, "journal_mode", "wal").unwrap();
    conn.execute_batch(&rule("walrus.example.org")).unwrap();
    let outcome = apply_pending(&s.here).unwrap();
    drop(conn);
    assert!(outcome.applied, "{}", outcome.message);
    assert_eq!(rules(&s.here), ["a.example.org", "new.example.org"]);
    assert!(!s.here.data().join("puddle.db-wal").exists());
}

#[test]
fn a_crash_after_the_marker_and_before_anything_else_is_finished_at_the_next_start() {
    let s = setup();
    request(&s.here, &s.staging_id, ApplyMode::Add, "").unwrap();
    // The state a crash during preparation leaves: the marker says Requested and `new/` is half built.
    let new = s.here.restore_pending().join("new");
    fs::create_dir_all(new.join("settings")).unwrap();
    fs::write(new.join("puddle.db"), b"half a database").unwrap();
    let outcome = apply_pending(&s.here).unwrap();
    assert!(outcome.applied, "{}", outcome.message);
    assert_eq!(
        rules(&s.here),
        ["a.example.org", "mine.example.org", "new.example.org"]
    );
}

#[test]
fn a_crash_in_the_middle_of_the_swap_is_finished_at_the_next_start() {
    let s = setup();
    request(&s.here, &s.staging_id, ApplyMode::Replace, "").unwrap();
    let pending = s.here.restore_pending();
    // Run the first half by hand: prepare, then swap only the database, as a crash after the
    // first rename would leave it.
    let mut marker: Marker =
        serde_json::from_slice(&fs::read(pending.join("marker.json")).unwrap()).unwrap();
    let (new, old) = (pending.join("new"), pending.join("old"));
    prepare(&s.here, &pending.join("staged"), &new, ApplyMode::Replace).unwrap();
    marker.phase = Phase::Swapping;
    write_json(&pending.join("marker.json"), &marker).unwrap();
    fs::create_dir_all(&old).unwrap();
    swap_part(&s.here, &new, &old, "puddle.db").unwrap();
    assert_eq!(
        rules(&s.here),
        ["a.example.org", "new.example.org"],
        "the database is already the backup's"
    );
    assert!(
        read(s.here.workspace_book())["workspaces"][0]["id"] == "w-here",
        "the list is still the old one"
    );

    let outcome = apply_pending(&s.here).unwrap();
    assert!(outcome.applied, "{}", outcome.message);
    assert_eq!(ids(&read(s.here.workspace_book())), ["w-there"]);
    let global = read(s.here.settings().join("global.json"));
    assert_eq!(global["c"].as_i64(), None);
    assert!(!pending.exists());
}

#[test]
fn a_crash_after_everything_was_swapped_only_removes_the_leftovers() {
    let s = setup();
    request(&s.here, &s.staging_id, ApplyMode::Replace, "").unwrap();
    let pending = s.here.restore_pending();
    let marker: Marker =
        serde_json::from_slice(&fs::read(pending.join("marker.json")).unwrap()).unwrap();
    run(&s.here, &pending, marker).unwrap();
    // `run` ended; the marker (and the leftovers) are still there, as a crash before cleanup leaves them.
    assert!(pending.join("marker.json").exists());
    let outcome = apply_pending(&s.here).unwrap();
    assert!(outcome.applied, "{}", outcome.message);
    assert_eq!(rules(&s.here), ["a.example.org", "new.example.org"]);
    assert!(!pending.exists());
}

#[test]
fn a_failed_preparation_starts_with_the_old_state_and_says_why() {
    let s = setup();
    let requested = request(&s.here, &s.staging_id, ApplyMode::Add, "").unwrap();
    // A workspace list in the backup that is not a list.
    fs::write(
        s.here
            .restore_pending()
            .join("staged")
            .join("workspaces.json"),
        b"[1, 2]",
    )
    .unwrap();
    let before = (rules(&s.here), fs::read(s.here.workspace_book()).unwrap());
    let outcome = apply_pending(&s.here).unwrap();
    assert!(!outcome.applied);
    assert!(
        outcome.message.contains("not a workspace list"),
        "{}",
        outcome.message
    );
    assert!(
        outcome.message.contains(&requested.safety_copy),
        "{}",
        outcome.message
    );
    assert_eq!(
        (rules(&s.here), fs::read(s.here.workspace_book()).unwrap()),
        before
    );
    assert!(!s.here.restore_pending().exists());
    assert!(s.here.safety_copies().join(&requested.safety_copy).is_dir());
}

#[test]
fn a_failed_swap_puts_the_moved_parts_back() {
    let s = setup();
    request(&s.here, &s.staging_id, ApplyMode::Replace, "").unwrap();
    let pending = s.here.restore_pending();
    let marker: Marker =
        serde_json::from_slice(&fs::read(pending.join("marker.json")).unwrap()).unwrap();
    let before = (
        rules(&s.here),
        fs::read(s.here.workspace_book()).unwrap(),
        fs::read(s.here.settings().join("global.json")).unwrap(),
    );
    // The database and the settings swap, then the workspace list fails: the first two go back.
    let failing = |paths: &HostPaths, new: &Path, old: &Path, part: &str| {
        if part == "workspaces.json" {
            return Err(RestoreError::Merge("the disk went away".into()));
        }
        swap_part(paths, new, old, part)
    };
    let err = run_with(&s.here, &pending, marker, failing).unwrap_err();
    assert!(err.to_string().contains("the disk went away"), "{err}");
    assert_eq!(
        (
            rules(&s.here),
            fs::read(s.here.workspace_book()).unwrap(),
            fs::read(s.here.settings().join("global.json")).unwrap()
        ),
        before
    );
    assert!(s.here.store().is_file());
}

#[test]
fn nothing_pending_changes_nothing_and_clears_unconfirmed_uploads() {
    let s = setup();
    assert!(s.here.restore_staging().join(&s.staging_id).is_dir());
    let before = rules(&s.here);
    assert!(apply_pending(&s.here).is_none());
    assert_eq!(rules(&s.here), before);
    assert!(
        !s.here.restore_staging().exists(),
        "an upload nobody confirmed does not outlive a start"
    );
    assert!(!s.here.restore_result().exists());
    // Leftovers of a request that never wrote its marker are cleared too.
    fs::create_dir_all(s.here.restore_pending().join("staged")).unwrap();
    assert!(apply_pending(&s.here).is_none());
    assert!(!s.here.restore_pending().exists());
}

#[test]
fn an_unreadable_marker_is_set_aside_and_nothing_is_applied() {
    let s = setup();
    request(&s.here, &s.staging_id, ApplyMode::Add, "").unwrap();
    fs::write(s.here.restore_pending().join("marker.json"), b"{not json").unwrap();
    let before = rules(&s.here);
    let outcome = apply_pending(&s.here).unwrap();
    assert!(!outcome.applied);
    assert_eq!(rules(&s.here), before);
    assert!(s.here.data().join("restore-pending.unreadable").is_dir());
    assert!(!s.here.restore_pending().exists());
}

#[cfg(unix)]
#[test]
fn restored_files_are_owner_only() {
    use std::os::unix::fs::PermissionsExt as _;
    let s = setup();
    request(&s.here, &s.staging_id, ApplyMode::Replace, "").unwrap();
    apply_pending(&s.here).unwrap();
    for path in [
        s.here.store(),
        s.here.settings().join("global.json"),
        s.here.workspace_book(),
    ] {
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o077;
        assert_eq!(mode, 0, "{} is open to others", path.display());
    }
    drop(s.root);
}

#[test]
fn utc_names_sort_by_time() {
    assert_eq!(utc_name(1_700_000_000_000), "20231114T221320Z");
}

#[test]
fn a_second_confirm_while_one_waits_is_refused_even_with_another_staged_copy() {
    let s = setup();
    request(&s.here, &s.staging_id, ApplyMode::Add, "").unwrap();
    // A second upload, staged while the first waits.
    let second = data_folder(s.root.path(), "second", &[], BOOK_THERE, "{}");
    let mut archive = crate::backup_archive(&second).unwrap();
    let mut bytes = Vec::new();
    io::Read::read_to_end(&mut archive.file, &mut bytes).unwrap();
    let staged = stage_archive(&bytes[..], &s.here.restore_staging()).unwrap();
    let err = request(&s.here, &staged.id, ApplyMode::Add, "").unwrap_err();
    assert!(matches!(err, RequestError::AlreadyPending), "{err}");
    assert!(err.to_string().contains("already waiting"), "{err}");
    assert!(s.here.restore_staging().join(&staged.id).is_dir());
}

#[test]
fn a_machine_with_no_database_takes_the_backups_as_it_is() {
    let s = setup();
    request(&s.here, &s.staging_id, ApplyMode::Add, "").unwrap();
    // The live database is gone (a data folder that never started).
    fs::remove_file(s.here.store()).unwrap();
    let outcome = apply_pending(&s.here).unwrap();
    assert!(outcome.applied, "{}", outcome.message);
    assert_eq!(rules(&s.here), ["a.example.org", "new.example.org"]);
}

#[test]
fn a_marker_that_cannot_be_read_at_all_is_set_aside_like_one_that_is_not_json() {
    let s = setup();
    request(&s.here, &s.staging_id, ApplyMode::Add, "").unwrap();
    let marker = s.here.restore_pending().join("marker.json");
    fs::remove_file(&marker).unwrap();
    // A folder where the marker should be: reading it fails with something other than "missing".
    fs::create_dir(&marker).unwrap();
    let before = rules(&s.here);
    let outcome = apply_pending(&s.here).unwrap();
    assert!(!outcome.applied);
    assert!(
        outcome.message.contains("marker could not be read"),
        "{}",
        outcome.message
    );
    assert_eq!(rules(&s.here), before);
}

#[test]
fn rolling_back_a_part_leaves_a_part_that_was_never_moved_alone() {
    let s = setup();
    let old = s.root.path().join("old");
    fs::create_dir_all(&old).unwrap();
    let before = fs::read(s.here.workspace_book()).unwrap();
    roll_back_part(&s.here, &old, "workspaces.json").unwrap();
    assert_eq!(fs::read(s.here.workspace_book()).unwrap(), before);
    // A part that was moved aside comes back over what is live (a file, or a whole folder).
    fs::rename(s.here.settings(), old.join("settings")).unwrap();
    fs::create_dir_all(s.here.settings().join("restored")).unwrap();
    roll_back_part(&s.here, &old, "settings").unwrap();
    assert!(s.here.settings().join("global.json").is_file());
    assert!(!s.here.settings().join("restored").exists());
    fs::rename(s.here.workspace_book(), old.join("workspaces.json")).unwrap();
    fs::write(s.here.workspace_book(), b"restored").unwrap();
    roll_back_part(&s.here, &old, "workspaces.json").unwrap();
    assert_eq!(fs::read(s.here.workspace_book()).unwrap(), before);
}
