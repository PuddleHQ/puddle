// SPDX-License-Identifier: AGPL-3.0-or-later
use serde_json::json;

use super::*;

/// An entry id the directory picks, so two lists name the same entry alike.
fn entry_for(dir: &str) -> EntryId {
    let mut bytes = [0_u8; 5];
    for (slot, b) in bytes.iter_mut().zip(dir.bytes().cycle()) {
        *slot = b;
    }
    EntryId::from_random(bytes)
}

fn repository(url: &str, dir: &str) -> Repository {
    Repository {
        entry: entry_for(dir),
        url: RepoUrl::parse(url).unwrap(),
        directory: CheckoutDir::new(dir).unwrap(),
    }
}

fn stored(id: &str) -> Stored {
    Stored {
        id: WorkspaceId::new(id).unwrap(),
        name: puddle_api::DisplayName::parse(id).unwrap(),
        repositories: vec![
            repository("https://example.org/a/b", "b"),
            repository("https://example.org/a/c.git", "see"),
        ],
        start_folder: Some(StartFolder::parse("see/src").unwrap()),
        session_user: SessionUser::Agent,
        image: "alpine:3".to_owned(),
        memory_mib: 1024,
        created_at: 7,
        disk_size_mib: 2048,
        root_disk_grown_mib: Some(32_768),
        creating: false,
        deleting: false,
    }
}

#[test]
fn settings_round_trip_and_a_missing_file_is_none() {
    let dir = tempfile::tempdir().unwrap();
    let repo = FileSettings::new(dir.path().join("settings"));
    let a = WorkspaceId::new("a").unwrap();
    assert_eq!(repo.load_global().unwrap(), None);
    assert_eq!(repo.load_workspace(&a).unwrap(), None);
    repo.save_global(json!({"schema_version": 1})).unwrap();
    repo.save_workspace(&a, json!({"overrides": {}})).unwrap();
    assert_eq!(
        repo.load_global().unwrap(),
        Some(json!({"schema_version": 1}))
    );
    assert_eq!(
        repo.load_workspace(&a).unwrap(),
        Some(json!({"overrides": {}}))
    );
    // A second instance over the same folder sees the same documents.
    let again = FileSettings::new(dir.path().join("settings"));
    assert_eq!(
        again.load_global().unwrap(),
        Some(json!({"schema_version": 1}))
    );
}

#[test]
fn deleting_a_workspace_document_removes_both_copies_and_a_missing_one_is_fine() {
    let dir = tempfile::tempdir().unwrap();
    let a = WorkspaceId::new("a").unwrap();
    let repo = FileSettings::new(dir.path());
    // Nothing stored: not an error.
    repo.delete_workspace(&a).unwrap();
    repo.save_workspace(&a, json!({"overrides": {"direct_ssh": true}}))
        .unwrap();
    let old = dir.path().join("sandboxes");
    std::fs::create_dir_all(&old).unwrap();
    std::fs::write(old.join("a.json"), br#"{"overrides":{"cpus":2}}"#).unwrap();
    repo.delete_workspace(&a).unwrap();
    assert!(!dir.path().join("workspaces").join("a.json").exists());
    assert!(!old.join("a.json").exists());
    assert_eq!(repo.load_workspace(&a).unwrap(), None);
    assert_eq!(
        FileSettings::new(dir.path()).load_workspace(&a).unwrap(),
        None
    );
}

#[test]
fn a_workspace_document_that_cannot_be_removed_is_an_error_and_the_other_copy_still_goes() {
    let dir = tempfile::tempdir().unwrap();
    let a = WorkspaceId::new("a").unwrap();
    let repo = FileSettings::new(dir.path());
    // A non-empty folder where the file belongs: removing it fails on every OS.
    let blocked = dir.path().join("workspaces").join("a.json");
    std::fs::create_dir_all(&blocked).unwrap();
    std::fs::write(blocked.join("keep"), b"x").unwrap();
    let old = dir.path().join("sandboxes");
    std::fs::create_dir_all(&old).unwrap();
    std::fs::write(old.join("a.json"), b"{}").unwrap();
    let err = repo.delete_workspace(&a).unwrap_err();
    assert!(err.to_string().contains("a.json"), "{err}");
    assert!(!old.join("a.json").exists());
}

#[test]
fn a_failed_delete_does_not_leave_the_cache_claiming_the_old_document() {
    let dir = tempfile::tempdir().unwrap();
    let a = WorkspaceId::new("a").unwrap();
    let repo = FileSettings::new(dir.path());
    repo.save_workspace(&a, json!({"overrides": {}})).unwrap();
    let blocked = dir.path().join("sandboxes").join("a.json");
    std::fs::create_dir_all(&blocked).unwrap();
    std::fs::write(blocked.join("keep"), b"x").unwrap();
    repo.delete_workspace(&a).unwrap_err();
    // The document is gone and the old copy cannot be read: the next load says so.
    repo.load_workspace(&a).unwrap_err();
}

#[test]
fn a_document_kept_under_the_old_folder_name_is_read_and_moves_on_save() {
    let dir = tempfile::tempdir().unwrap();
    let old = dir.path().join("sandboxes");
    std::fs::create_dir_all(&old).unwrap();
    std::fs::write(old.join("a.json"), br#"{"overrides":{"cpus":2}}"#).unwrap();
    let a = WorkspaceId::new("a").unwrap();
    let repo = FileSettings::new(dir.path());
    assert_eq!(
        repo.load_workspace(&a).unwrap(),
        Some(json!({"overrides": {"cpus": 2}}))
    );
    repo.save_workspace(&a, json!({"overrides": {"cpus": 4}}))
        .unwrap();
    assert!(!old.join("a.json").exists());
    assert!(dir.path().join("workspaces").join("a.json").exists());
    let again = FileSettings::new(dir.path());
    assert_eq!(
        again.load_workspace(&a).unwrap(),
        Some(json!({"overrides": {"cpus": 4}}))
    );
}

#[test]
fn a_damaged_settings_file_is_an_error_not_an_empty_document() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("global.json"), b"{ nope").unwrap();
    let err = FileSettings::new(dir.path()).load_global().unwrap_err();
    assert!(err.to_string().contains("not valid JSON"), "{err}");
}

#[test]
fn the_app_icon_round_trips_survives_a_restart_and_deletes() {
    let dir = tempfile::tempdir().unwrap();
    let repo = FileSettings::new(dir.path().join("settings"));
    assert_eq!(repo.load_app_icon().unwrap(), None);
    repo.delete_app_icon().unwrap();
    repo.save_app_icon(b"one").unwrap();
    assert_eq!(repo.load_app_icon().unwrap(), Some(b"one".to_vec()));
    let again = FileSettings::new(dir.path().join("settings"));
    assert_eq!(again.load_app_icon().unwrap(), Some(b"one".to_vec()));
    repo.save_app_icon(b"two").unwrap();
    assert_eq!(repo.load_app_icon().unwrap(), Some(b"two".to_vec()));
    repo.delete_app_icon().unwrap();
    assert_eq!(repo.load_app_icon().unwrap(), None);
    assert!(!dir.path().join("settings").join("app-icon.png").exists());
}

#[test]
fn an_app_icon_that_cannot_be_read_written_or_removed_says_so() {
    let dir = tempfile::tempdir().unwrap();
    // A folder where the file belongs: reading and removing it fail.
    std::fs::create_dir(dir.path().join("app-icon.png")).unwrap();
    let repo = FileSettings::new(dir.path());
    assert!(
        repo.load_app_icon()
            .unwrap_err()
            .to_string()
            .contains("cannot read")
    );
    assert!(
        repo.delete_app_icon()
            .unwrap_err()
            .to_string()
            .contains("cannot remove")
    );
    assert!(
        repo.save_app_icon(b"x")
            .unwrap_err()
            .to_string()
            .contains("cannot write")
    );
}

#[test]
fn a_settings_folder_that_is_a_file_cannot_be_written() {
    let dir = tempfile::tempdir().unwrap();
    let blocker = dir.path().join("settings");
    std::fs::write(&blocker, b"x").unwrap();
    let err = FileSettings::new(&blocker)
        .save_global(json!({}))
        .unwrap_err();
    assert!(err.to_string().contains("cannot write"), "{err}");
}

#[test]
fn the_book_round_trips_and_a_missing_book_is_empty() {
    let dir = tempfile::tempdir().unwrap();
    let book = WorkspaceBook::new(dir.path().join("workspaces.json"));
    assert_eq!(book.load().unwrap().workspaces, Vec::new());
    book.save(vec![stored("a"), stored("b")], &BTreeSet::new())
        .unwrap();
    assert_eq!(
        book.load().unwrap().workspaces,
        vec![stored("a"), stored("b")]
    );
}

#[test]
fn a_damaged_or_newer_book_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("workspaces.json");
    let book = WorkspaceBook::new(&path);
    std::fs::write(&path, b"[1,2").unwrap();
    assert!(
        book.load()
            .unwrap_err()
            .to_string()
            .contains("not valid JSON")
    );
    std::fs::write(&path, b"{\"nope\": true}").unwrap();
    assert!(
        book.load()
            .unwrap_err()
            .to_string()
            .contains("not a workspace list")
    );
    std::fs::write(&path, br#"{"version": 99, "workspaces": []}"#).unwrap();
    assert!(
        book.load()
            .unwrap_err()
            .to_string()
            .contains("newer Puddle")
    );
    std::fs::write(&path, br#"{"version": 4, "workspaces": []}"#).unwrap();
    assert!(
        book.load()
            .unwrap_err()
            .to_string()
            .contains("written by a newer Puddle (version 4)")
    );
    std::fs::write(&path, br#"{"version": 0, "workspaces": []}"#).unwrap();
    assert!(
        book.load()
            .unwrap_err()
            .to_string()
            .contains("no Puddle wrote")
    );
    let bad = json!({"version": 1, "workspaces": [{
        "id": "Not A Name", "name": "x", "repo_url": "u", "image": "i", "memory_mib": 1,
        "created_at": 0, "disk_size_mib": 1, "first_connect_notice_due": false
    }]});
    std::fs::write(&path, bad.to_string()).unwrap();
    assert!(book.load().is_err());
    for (field, value, why) in [
        ("url", json!("git@h:a/b"), "bad repository address"),
        ("directory", json!("../up"), "bad checkout directory"),
    ] {
        let mut entry = json!({"url": "https://h/a/b", "directory": "b"});
        entry[field] = value;
        let bad = json!({"version": 2, "workspaces": [{
            "id": "a", "name": "a", "repositories": [entry], "start_folder": null,
            "image": "i", "memory_mib": 512, "created_at": 0, "disk_size_mib": 1
        }]});
        std::fs::write(&path, bad.to_string()).unwrap();
        let err = book.load().unwrap_err().to_string();
        assert!(err.contains(why), "{err}");
    }
    let bad = json!({"version": 2, "workspaces": [{
        "id": "a", "name": "a", "repositories": [], "start_folder": "/etc",
        "image": "i", "memory_mib": 512, "created_at": 0, "disk_size_mib": 1
    }]});
    std::fs::write(&path, bad.to_string()).unwrap();
    assert!(
        book.load()
            .unwrap_err()
            .to_string()
            .contains("bad start folder")
    );
}

/// A version 1 book as Puddle wrote it before repository lists: one entry still creating, one
/// finished from before the creating flag existed (with the retired first-connect flag), one
/// whose address gave no usable directory name.
const V1_BOOK: &str = r#"{
  "version": 1,
  "workspaces": [
    {
      "id": "shop",
      "name": "shop",
      "display_name": "Web shop",
      "repo_url": "https://github.com/acme/shop.git",
      "image": "mcr.microsoft.com/devcontainers/base:debian",
      "memory_mib": 8192,
      "created_at": 1760000000000,
      "disk_size_mib": 32768,
      "creating": false
    },
    {
      "id": "old",
      "name": "old",
      "repo_url": "https://example.org/team/tools",
      "image": "alpine:3",
      "memory_mib": 512,
      "created_at": 1,
      "disk_size_mib": 64,
      "first_connect_notice_due": true
    },
    {
      "id": "odd",
      "name": "odd",
      "repo_url": "https://example.org/team/.hidden",
      "image": "alpine:3",
      "memory_mib": 512,
      "created_at": 2,
      "disk_size_mib": 64,
      "creating": true
    }
  ]
}"#;

/// Per workspace: id, (address, directory) of each repository, start folder, creating.
type Shape = Vec<(String, Vec<(String, String)>, Option<String>, bool)>;

fn shape(loaded: &[Stored]) -> Shape {
    loaded
        .iter()
        .map(|s| {
            (
                s.id.to_string(),
                s.repositories
                    .iter()
                    .map(|r| (r.url.to_string(), r.directory.to_string()))
                    .collect(),
                s.start_folder.as_ref().map(ToString::to_string),
                s.creating,
            )
        })
        .collect()
}

fn v1_shape() -> Shape {
    let one = |url: &str, dir: &str| vec![(url.to_owned(), dir.to_owned())];
    vec![
        (
            "shop".into(),
            one("https://github.com/acme/shop.git", "shop"),
            Some("shop".into()),
            false,
        ),
        (
            "old".into(),
            one("https://example.org/team/tools", "tools"),
            Some("tools".into()),
            false,
        ),
        (
            "odd".into(),
            one("https://example.org/team/.hidden", "src"),
            Some("src".into()),
            true,
        ),
    ]
}

#[test]
fn a_version_1_book_loads_as_lists_with_the_checkout_as_start_folder_and_is_never_written() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("workspaces.json");
    std::fs::write(&path, V1_BOOK).unwrap();
    let before = std::fs::metadata(&path).unwrap().modified().unwrap();
    let book = WorkspaceBook::new(&path);
    let loaded = book.load().unwrap().workspaces;
    assert_eq!(shape(&loaded), v1_shape());
    let shop = &loaded[0];
    assert_eq!(shop.name.as_str(), "Web shop");
    assert_eq!(loaded[1].name.as_str(), "old");
    assert_eq!(
        (
            shop.image.as_str(),
            shop.memory_mib,
            shop.created_at,
            shop.disk_size_mib
        ),
        (
            "mcr.microsoft.com/devcontainers/base:debian",
            8192,
            1_760_000_000_000,
            32768
        )
    );
    book.load().unwrap();
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        V1_BOOK,
        "a read never writes"
    );
    assert_eq!(
        std::fs::metadata(&path).unwrap().modified().unwrap(),
        before
    );

    // The next save writes version 3, without the old field, and it reads back the same.
    book.save(loaded.clone(), &BTreeSet::new()).unwrap();
    let written: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(written["version"], 3);
    assert_eq!(written["workspaces"][0]["name"], "Web shop");
    let mut saved = written["workspaces"][0]["repositories"].clone();
    let entry = saved[0].as_object_mut().unwrap().remove("entry").unwrap();
    assert!(EntryId::new(entry.as_str().unwrap()).is_ok(), "{entry}");
    assert_eq!(
        saved,
        json!([{"url": "https://github.com/acme/shop.git", "directory": "shop"}])
    );
    assert_eq!(written["workspaces"][0]["start_folder"], "shop");
    for entry in written["workspaces"].as_array().unwrap() {
        assert!(entry.get("repo_url").is_none(), "{entry}");
        assert!(entry.get("display_name").is_none(), "{entry}");
        assert!(entry.get("first_connect_notice_due").is_none(), "{entry}");
    }
    assert_eq!(book.load().unwrap().workspaces, loaded);
}

#[test]
fn an_entry_from_before_session_users_keeps_root_and_the_user_is_saved() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("workspaces.json");
    let book = WorkspaceBook::new(&path);
    let old = json!({"version": 2, "workspaces": [{
        "id": "a", "name": "a", "repositories": [], "start_folder": null,
        "image": "i", "memory_mib": 512, "created_at": 0, "disk_size_mib": 1
    }]});
    std::fs::write(&path, old.to_string()).unwrap();
    let loaded = book.load().unwrap().workspaces;
    assert_eq!(loaded[0].session_user, SessionUser::Root);
    book.save(vec![loaded[0].clone(), stored("b")], &BTreeSet::new())
        .unwrap();
    let written: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(written["workspaces"][0]["session_user"], "root");
    assert_eq!(written["workspaces"][1]["session_user"], "agent");
    let bad = json!({"version": 2, "workspaces": [{
        "id": "a", "name": "a", "repositories": [], "session_user": "vscode",
        "image": "i", "memory_mib": 512, "created_at": 0, "disk_size_mib": 1
    }]});
    std::fs::write(&path, bad.to_string()).unwrap();
    assert!(book.load().is_err());
    // A version 1 book is older still: every workspace in it keeps root.
    std::fs::write(&path, V1_BOOK).unwrap();
    assert!(
        book.load()
            .unwrap()
            .workspaces
            .iter()
            .all(|w| w.session_user == SessionUser::Root)
    );
}

#[test]
fn a_version_1_book_written_by_a_puddle_before_repository_lists_loads() {
    // Written by `WorkspaceBook::save` of the version before repository lists, unchanged.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("workspaces.json");
    std::fs::write(
        &path,
        include_str!("../../tests/fixtures/workspaces-v1.json"),
    )
    .unwrap();
    let loaded = WorkspaceBook::new(&path).load().unwrap().workspaces;
    assert_eq!(shape(&loaded), v1_shape());
}

#[test]
fn a_version_2_book_written_by_the_previous_release_loads_in_place_and_saves_as_version_3() {
    // Written by `WorkspaceBook::save` of the release before display text names, unchanged.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("workspaces.json");
    let old = include_str!("../../tests/fixtures/workspaces-v2.json");
    std::fs::write(&path, old).unwrap();
    let book = WorkspaceBook::new(&path);
    let loaded = book.load().unwrap().workspaces;
    // Each entry keeps its id; the name is the old display name, else the old name.
    assert_eq!(
        loaded
            .iter()
            .map(|s| (s.id.as_str(), s.name.as_str()))
            .collect::<Vec<_>>(),
        [("shop", "Shop API \u{fc}"), ("tools", "tools")]
    );
    assert_eq!(loaded[0].repositories.len(), 2);
    assert_eq!(loaded[0].memory_mib, 8192);
    assert_eq!(loaded[1].session_user, SessionUser::Root);
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        old,
        "a read never writes"
    );
    book.save(loaded.clone(), &BTreeSet::new()).unwrap();
    let written: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(written["version"], 3);
    assert_eq!(written["workspaces"][0]["id"], "shop");
    assert_eq!(written["workspaces"][0]["name"], "Shop API \u{fc}");
    assert!(written["workspaces"][0].get("display_name").is_none());
    assert_eq!(book.load().unwrap().workspaces, loaded);
    // A display name that is no longer valid text stops the start with its workspace named.
    let bad = old.replace("Shop API \u{fc}", "two\\nlines");
    std::fs::write(&path, bad).unwrap();
    let err = book.load().unwrap_err().to_string();
    assert!(
        err.contains("\"shop\"") && err.contains("bad name"),
        "{err}"
    );
}

#[test]
fn a_version_3_book_round_trips_with_empty_and_long_lists() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("workspaces.json");
    let book = WorkspaceBook::new(&path);
    let mut empty = stored("empty");
    empty.repositories.clear();
    empty.start_folder = None;
    let mut many = stored("many");
    many.repositories = (0..100)
        .map(|n| repository(&format!("https://h/a/r{n}"), &format!("r{n}")))
        .collect();
    let all = vec![stored("a"), empty, many];
    book.save(all.clone(), &BTreeSet::new()).unwrap();
    assert_eq!(book.load().unwrap().workspaces, all);
    let written: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(written["workspaces"][1]["repositories"], json!([]));
    assert_eq!(written["workspaces"][1]["start_folder"], Value::Null);
}

#[test]
fn entries_of_an_older_list_get_ids_on_read_and_keep_them_once_saved() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("workspaces.json");
    let old = include_str!("../../tests/fixtures/workspaces-v2.json");
    std::fs::write(&path, old).unwrap();
    let book = WorkspaceBook::new(&path);
    let first = book.load().unwrap().workspaces;
    let ids: Vec<&EntryId> = first
        .iter()
        .flat_map(|s| s.repositories.iter().map(|r| &r.entry))
        .collect();
    let unique: BTreeSet<_> = ids.iter().collect();
    assert_eq!(unique.len(), ids.len(), "one id per entry, none twice");
    assert_ne!(ids.len(), 0);
    book.save(first.clone(), &BTreeSet::new()).unwrap();
    assert_eq!(
        book.load().unwrap().workspaces,
        first,
        "saved ids are read back"
    );
    let written: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(written["version"], 3);
    assert_eq!(
        written["workspaces"][0]["repositories"][0]["entry"],
        first[0].repositories[0].entry.as_str()
    );
}

#[test]
fn a_list_with_a_bad_or_repeated_entry_id_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("workspaces.json");
    let book = WorkspaceBook::new(&path);
    for (entries, why) in [
        (
            json!([
                {"entry": "a0000000", "url": "https://h/a/b", "directory": "b"},
                {"entry": "a0000000", "url": "https://h/a/c", "directory": "c"}
            ]),
            "one repository entry id twice",
        ),
        (
            json!([{"entry": "b", "url": "https://h/a/b", "directory": "b"}]),
            "bad repository entry id",
        ),
    ] {
        let bad = json!({"version": 3, "workspaces": [{
            "id": "a", "name": "a", "repositories": entries, "start_folder": null,
            "image": "i", "memory_mib": 512, "created_at": 0, "disk_size_mib": 1
        }]});
        std::fs::write(&path, bad.to_string()).unwrap();
        let err = book.load().unwrap_err().to_string();
        assert!(err.contains(why), "{err}");
    }
}

#[test]
fn retired_ids_are_kept_beside_the_workspaces_and_a_bad_one_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("workspaces.json");
    let book = WorkspaceBook::new(&path);
    let retired: BTreeSet<WorkspaceId> = ["acme", "k3m9q2xa"]
        .into_iter()
        .map(|id| WorkspaceId::new(id).unwrap())
        .collect();
    book.save(vec![stored("a")], &retired).unwrap();
    let loaded = book.load().unwrap();
    assert_eq!(loaded.workspaces, vec![stored("a")]);
    assert_eq!(loaded.retired, retired);
    let written: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(written["retired_ids"], json!(["acme", "k3m9q2xa"]));
    // None retired: the field is left out, and a book without it has none.
    book.save(vec![stored("a")], &BTreeSet::new()).unwrap();
    let written: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(written.get("retired_ids"), None);
    assert!(book.load().unwrap().retired.is_empty());
    std::fs::write(
        &path,
        br#"{"version": 3, "workspaces": [], "retired_ids": ["Not An Id"]}"#,
    )
    .unwrap();
    let err = book.load().unwrap_err().to_string();
    assert!(
        err.contains("retired id \"Not An Id\" is not an id"),
        "{err}"
    );
}
