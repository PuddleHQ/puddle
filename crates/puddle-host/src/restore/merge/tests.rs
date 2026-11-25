// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

fn restored() -> RestoredDatabase {
    // The backup's owner (1) is this machine's owner (7); its user 2 is this machine's user 9.
    RestoredDatabase {
        user_map: vec![(1, 7), (2, 9)],
        owner_id: 7,
    }
}

fn write(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, body).unwrap();
    path
}

fn read(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

#[test]
fn add_keeps_a_present_setting_and_adds_a_missing_one() {
    let dir = tempfile::tempdir().unwrap();
    let here = dir.path().join("here");
    let there = dir.path().join("there");
    write(&here, "global.json", r#"{"version":1,"a":9,"c":3}"#);
    write(&there, "global.json", r#"{"version":2,"a":1,"b":2}"#);
    write(&there, "workspaces/w.json", r#"{"version":1,"x":1}"#);
    write(&there, "workspaces/skip.json.tmp", "half");
    let new = dir.path().join("new");
    settings(ApplyMode::Add, &here, &there, &new).unwrap();
    let global = read(&new.join("global.json"));
    assert_eq!(
        (
            global["version"].as_i64(),
            global["a"].as_i64(),
            global["b"].as_i64(),
            global["c"].as_i64()
        ),
        (Some(1), Some(9), Some(2), Some(3)),
        "the present version and keys win"
    );
    assert!(
        new.join("workspaces/w.json").is_file(),
        "a document that is missing is added whole"
    );
    assert!(!new.join("workspaces/skip.json.tmp").exists());
    // The present folder is as it was.
    assert_eq!(read(&here.join("global.json"))["b"], Value::Null);
}

#[test]
fn replace_takes_the_backups_documents_only() {
    let dir = tempfile::tempdir().unwrap();
    let here = dir.path().join("here");
    let there = dir.path().join("there");
    write(&here, "global.json", r#"{"version":1,"c":3}"#);
    write(&here, "only-here.json", "{}");
    write(&there, "global.json", r#"{"version":1,"a":1}"#);
    let new = dir.path().join("new");
    settings(ApplyMode::Replace, &here, &there, &new).unwrap();
    assert_eq!(read(&new.join("global.json"))["a"], 1);
    assert_eq!(read(&new.join("global.json"))["c"], Value::Null);
    assert!(!new.join("only-here.json").exists());
}

#[test]
fn the_backups_workspaces_are_owned_by_the_users_they_map_to() {
    let dir = tempfile::tempdir().unwrap();
    let here = write(
        dir.path(),
        "here.json",
        r#"{"version":4,"workspaces":[{"id":"ws-one","name":"One","owner":7}]}"#,
    );
    let there = write(
        dir.path(),
        "there.json",
        r#"{"version":4,"workspaces":[{"id":"ws-one","name":"Theirs","owner":2},{"id":"ws-two","name":"Two","owner":2},{"id":"ws-three","name":"Three","owner":1},{"id":"ws-four","name":"Four","owner":99}]}"#,
    );
    let new = dir.path().join("new.json");
    book(ApplyMode::Add, &here, &there, &new, &restored()).unwrap();
    let merged = read(&new);
    let owners: Vec<(String, i64)> = entries_of(merged.as_object().unwrap())
        .iter()
        .map(|e| {
            (
                e["id"].as_str().unwrap().to_owned(),
                e["owner"].as_i64().unwrap(),
            )
        })
        .collect();
    // ws-one exists here and is kept as it is; the rest follow the user map (an unknown user: the owner).
    assert_eq!(
        owners,
        [
            ("ws-one".into(), 7),
            ("ws-two".into(), 9),
            ("ws-three".into(), 7),
            ("ws-four".into(), 7)
        ]
    );
    assert_eq!(merged["workspaces"][0]["name"], "One");
}

#[test]
fn retired_ids_of_both_sides_stay_retired() {
    let dir = tempfile::tempdir().unwrap();
    let here = write(
        dir.path(),
        "here.json",
        r#"{"version":4,"workspaces":[],"retired_ids":["w-a"]}"#,
    );
    let there = write(
        dir.path(),
        "there.json",
        r#"{"version":4,"workspaces":[],"retired_ids":["w-b","w-a"]}"#,
    );
    for mode in [ApplyMode::Add, ApplyMode::Replace] {
        let new = dir.path().join(format!("new-{mode:?}.json"));
        book(mode, &here, &there, &new, &restored()).unwrap();
        assert_eq!(
            read(&new)["retired_ids"],
            serde_json::json!(["w-a", "w-b"]),
            "{mode:?}"
        );
    }
}

#[test]
fn lists_in_formats_three_and_four_merge_into_format_four_with_an_owner_each() {
    let dir = tempfile::tempdir().unwrap();
    let v4 = r#"{"version":4,"workspaces":[{"id":"ws-one","name":"One","owner":7}]}"#;
    let v3 = r#"{"version":3,"workspaces":[{"id":"ws-two","name":"Two"}]}"#;
    // A backup in format 3 into a machine in format 4, and the other way round.
    for (name, here, there) in [("a", v4, v3), ("b", v3, v4)] {
        let here = write(dir.path(), &format!("here-{name}.json"), here);
        let there = write(dir.path(), &format!("there-{name}.json"), there);
        check_book_versions(&here, &there).unwrap();
        let new = dir.path().join(format!("new-{name}.json"));
        book(ApplyMode::Add, &here, &there, &new, &restored()).unwrap();
        let merged = read(&new);
        assert_eq!(merged["version"], 4, "{merged}");
        let mut owners: Vec<(String, i64)> = entries_of(merged.as_object().unwrap())
            .iter()
            .map(|e| {
                (
                    e["id"].as_str().unwrap().to_owned(),
                    e["owner"].as_i64().unwrap(),
                )
            })
            .collect();
        owners.sort();
        assert_eq!(
            owners,
            [("ws-one".into(), 7), ("ws-two".into(), 7)],
            "{merged}"
        );
    }
}

#[test]
fn lists_in_a_format_older_than_three_are_refused_and_the_message_says_why() {
    let dir = tempfile::tempdir().unwrap();
    let here = write(
        dir.path(),
        "here.json",
        r#"{"version":4,"workspaces":[{"id":"ws-one","name":"One","owner":7}]}"#,
    );
    let there = write(
        dir.path(),
        "there.json",
        r#"{"version":2,"workspaces":[{"id":"ws-two","name":"Two"}]}"#,
    );
    let err = check_book_versions(&here, &there).unwrap_err().to_string();
    assert!(
        err.contains("format 2") && err.contains("format 4"),
        "{err}"
    );
    assert!(
        err.contains("the backup's format 2 describes workspaces differently"),
        "{err}"
    );
    assert!(err.contains("choose Replace"), "{err}");
    // The same the other way round names this machine's list.
    let err = check_book_versions(&there, &here).unwrap_err().to_string();
    assert!(err.contains("this machine's format 2"), "{err}");
    let err = book(
        ApplyMode::Add,
        &here,
        &there,
        &dir.path().join("new.json"),
        &restored(),
    )
    .unwrap_err();
    assert!(matches!(err, RestoreError::Merge(_)), "{err}");
    // An empty list on either side is no obstacle, and Replace never merges.
    let empty = write(dir.path(), "empty.json", r#"{"version":4,"workspaces":[]}"#);
    check_book_versions(&empty, &there).unwrap();
    book(
        ApplyMode::Replace,
        &here,
        &there,
        &dir.path().join("replaced.json"),
        &restored(),
    )
    .unwrap();
    assert_eq!(
        read(&dir.path().join("replaced.json"))["workspaces"][0]["id"],
        "ws-two"
    );
}

#[test]
fn a_backup_without_a_list_leaves_the_present_list_alone_and_a_bad_one_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let here = write(dir.path(), "here.json", r#"{"version":4,"workspaces":[]}"#);
    let new = dir.path().join("new.json");
    book(
        ApplyMode::Replace,
        &here,
        &dir.path().join("missing.json"),
        &new,
        &restored(),
    )
    .unwrap();
    assert!(!new.exists());
    let bad = write(dir.path(), "bad.json", "[1]");
    let err = book(ApplyMode::Add, &here, &bad, &new, &restored()).unwrap_err();
    assert!(err.to_string().contains("not a workspace list"), "{err}");
}

#[test]
fn a_backup_with_no_settings_folder_adds_and_replaces_nothing_there() {
    let dir = tempfile::tempdir().unwrap();
    let here = dir.path().join("here");
    write(&here, "global.json", r#"{"a":1}"#);
    let nothing = dir.path().join("nothing");
    let new = dir.path().join("new");
    settings(ApplyMode::Add, &here, &nothing, &new).unwrap();
    assert_eq!(read(&new.join("global.json"))["a"], 1);
    let replaced = dir.path().join("replaced");
    settings(ApplyMode::Replace, &here, &nothing, &replaced).unwrap();
    assert!(!replaced.exists());
}

#[test]
fn nested_present_folders_are_copied_and_a_document_that_is_not_json_is_kept() {
    let dir = tempfile::tempdir().unwrap();
    let here = dir.path().join("here");
    let there = dir.path().join("there");
    write(&here, "workspaces/deep/w.json", r#"{"a":1}"#);
    write(&here, "notes.txt", "mine");
    write(&there, "notes.txt", "theirs");
    write(&there, "workspaces/deep/w.json", "not json at all");
    let new = dir.path().join("new");
    settings(ApplyMode::Add, &here, &there, &new).unwrap();
    assert_eq!(fs::read_to_string(new.join("notes.txt")).unwrap(), "mine");
    assert_eq!(read(&new.join("workspaces/deep/w.json"))["a"], 1);
}

#[test]
fn a_document_that_cannot_be_read_back_is_an_io_error() {
    let dir = tempfile::tempdir().unwrap();
    let here = dir.path().join("here");
    let there = dir.path().join("there");
    write(&here, "a.json", "{}");
    write(&there, "a.json", r#"{"b":1}"#);
    let new = dir.path().join("new");
    // A folder where the merged document goes: reading it back fails with something other than missing.
    fs::create_dir_all(new.join("a.json")).unwrap();
    let err = add_missing(&there, Path::new(""), &new).unwrap_err();
    assert!(matches!(err, RestoreError::Io { .. }), "{err}");
}

#[test]
fn a_workspace_list_that_cannot_be_read_is_named() {
    let dir = tempfile::tempdir().unwrap();
    // A folder where the list should be.
    let err = read_book(dir.path()).unwrap_err().to_string();
    assert!(err.contains("cannot read"), "{err}");
}

#[test]
fn a_present_list_without_workspaces_takes_the_backups_whole() {
    let dir = tempfile::tempdir().unwrap();
    let here = write(dir.path(), "here.json", r#"{"version":4,"workspaces":[]}"#);
    let there = write(
        dir.path(),
        "there.json",
        r#"{"version":4,"workspaces":[{"id":"ws-x","name":"X","owner":1}]}"#,
    );
    let new = dir.path().join("new.json");
    book(ApplyMode::Add, &here, &there, &new, &restored()).unwrap();
    assert_eq!(read(&new)["workspaces"][0]["owner"], 7);
    // No list here at all.
    let new = dir.path().join("new2.json");
    book(
        ApplyMode::Add,
        &dir.path().join("missing.json"),
        &there,
        &new,
        &restored(),
    )
    .unwrap();
    assert_eq!(read(&new)["workspaces"][0]["id"], "ws-x");
}

#[test]
fn two_documents_that_are_not_objects_are_left_as_they_are() {
    assert!(merge_documents(b"[1]", b"[2]").is_none());
    assert!(merge_documents(b"{}", b"{}").is_none());
    assert!(merge_documents(b"{\"a\":1}", b"not json").is_none());
    let merged = merge_documents(b"{\"a\":1}", b"{\"b\":2}").unwrap();
    assert_eq!(serde_json::from_slice::<Value>(&merged).unwrap()["b"], 2);
}

#[test]
fn a_present_temp_file_is_not_copied() {
    let dir = tempfile::tempdir().unwrap();
    let here = dir.path().join("here");
    write(&here, "global.json", "{}");
    write(&here, ".global.json.1.tmp", "half");
    let new = dir.path().join("new");
    settings(ApplyMode::Add, &here, &dir.path().join("nothing"), &new).unwrap();
    assert!(new.join("global.json").is_file());
    assert!(!new.join(".global.json.1.tmp").exists());
}

#[test]
fn a_restore_never_takes_over_the_first_run_flag() {
    for mode in [ApplyMode::Add, ApplyMode::Replace] {
        let dir = tempfile::tempdir().unwrap();
        let here = dir.path().join("here");
        let there = dir.path().join("there");
        write(&here, "global.json", r#"{"version":1}"#);
        write(
            &there,
            "global.json",
            r#"{"version":1,"first_run":{"completed_at":5},"a":1}"#,
        );
        let new = dir.path().join("new");
        settings(mode, &here, &there, &new).unwrap();
        let global = read(&new.join("global.json"));
        assert_eq!(
            global["first_run"],
            Value::Null,
            "the flag stays unset here"
        );
        assert_eq!(
            global["a"].as_i64(),
            Some(1),
            "{mode:?}: the other settings still come over"
        );

        // A machine that has been through the flow keeps its own value.
        write(
            &here,
            "global.json",
            r#"{"version":1,"first_run":{"completed_at":9}}"#,
        );
        let again = dir.path().join("again");
        settings(mode, &here, &there, &again).unwrap();
        assert_eq!(
            read(&again.join("global.json"))["first_run"]["completed_at"],
            9
        );
    }
}
