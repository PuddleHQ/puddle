// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

struct Rig {
    dir: tempfile::TempDir,
    server: EditorServer,
}

impl Rig {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        let plan = dir.path().join("plan.json");
        let server = EditorServer::new(data.to_str().unwrap(), "/f", 300)
            .with_defaults_file(plan.to_str().unwrap());
        std::fs::create_dir_all(data.join("user-data/User")).unwrap();
        std::fs::create_dir_all(data.join("extensions")).unwrap();
        Self { dir, server }
    }

    fn plan(&self, defaults: &EditorDefaults) {
        std::fs::write(
            self.dir.path().join("plan.json"),
            serde_json::to_string(defaults).unwrap(),
        )
        .unwrap();
    }

    fn unplan(&self) {
        std::fs::remove_file(self.dir.path().join("plan.json")).unwrap();
    }

    fn user(&self, name: &str) -> PathBuf {
        self.dir.path().join("data/user-data/User").join(name)
    }

    fn settings(&self) -> Value {
        let bytes = std::fs::read(self.user("settings.json")).unwrap();
        parse_merge_file(MergeFormat::Jsonc, &bytes).unwrap()
    }
}

fn dark(size: Option<u8>) -> EditorDefaults {
    EditorDefaults {
        colour_theme: Some("Solarized Dark".to_owned()),
        font_size: size,
        ..EditorDefaults::default()
    }
}

#[test]
fn nothing_planned_and_nothing_applied_touches_nothing() {
    let rig = Rig::new();
    std::fs::write(rig.user("settings.json"), "// mine\n{\"a\": 1}\n").unwrap();
    assert_eq!(apply(&rig.server), Outcome::default());
    assert_eq!(
        std::fs::read_to_string(rig.user("settings.json")).unwrap(),
        "// mine\n{\"a\": 1}\n"
    );
    assert!(!state_path(&rig.server).exists());
}

#[test]
fn the_theme_and_size_are_merged_and_the_users_comments_stay() {
    let rig = Rig::new();
    std::fs::write(
        rig.user("settings.json"),
        "{\n  // keep me\n  \"editor.tabSize\": 2,\n  \"window.autoDetectColorScheme\": true\n}\n",
    )
    .unwrap();
    rig.plan(&dark(Some(16)));
    apply(&rig.server);
    let text = std::fs::read_to_string(rig.user("settings.json")).unwrap();
    assert!(text.contains("// keep me"), "{text}");
    let settings = rig.settings();
    assert_eq!(settings["workbench.colorTheme"], "Solarized Dark");
    assert_eq!(settings["editor.fontSize"], 16);
    assert_eq!(settings["window.autoDetectColorScheme"], false);
    assert_eq!(settings["editor.tabSize"], 2);
}

#[test]
fn a_key_the_user_changed_stays_and_an_untouched_key_follows_a_new_default() {
    let rig = Rig::new();
    rig.plan(&dark(Some(16)));
    apply(&rig.server);
    // The user picks their own theme; the size is left alone.
    let mut settings = rig.settings();
    settings["workbench.colorTheme"] = json!("Monokai");
    std::fs::write(rig.user("settings.json"), settings.to_string()).unwrap();
    rig.plan(&EditorDefaults {
        colour_theme: Some("Abyss".to_owned()),
        font_size: Some(18),
        ..EditorDefaults::default()
    });
    apply(&rig.server);
    let settings = rig.settings();
    assert_eq!(
        settings["workbench.colorTheme"], "Monokai",
        "the user's choice wins"
    );
    assert_eq!(
        settings["editor.fontSize"], 18,
        "an untouched key follows the default"
    );
}

#[test]
fn an_unset_default_is_handed_back_only_where_the_user_did_not_change_it() {
    let rig = Rig::new();
    rig.plan(&dark(Some(16)));
    apply(&rig.server);
    let mut settings = rig.settings();
    settings["editor.fontSize"] = json!(20);
    std::fs::write(rig.user("settings.json"), settings.to_string()).unwrap();
    rig.plan(&EditorDefaults::default());
    apply(&rig.server);
    let settings = rig.settings();
    assert!(settings.get("workbench.colorTheme").is_none(), "{settings}");
    assert_eq!(
        settings["window.autoDetectColorScheme"], true,
        "the system scheme again"
    );
    assert_eq!(settings["editor.fontSize"], 20, "the user's own size stays");
}

#[test]
fn a_settings_file_that_does_not_parse_is_left_alone() {
    let rig = Rig::new();
    std::fs::write(rig.user("settings.json"), "{ not json").unwrap();
    rig.plan(&dark(None));
    apply(&rig.server);
    assert_eq!(
        std::fs::read_to_string(rig.user("settings.json")).unwrap(),
        "{ not json"
    );
}

fn bindings(list: &[(&str, &str)]) -> String {
    serde_json::to_string(
        &list
            .iter()
            .map(|(k, c)| json!({"key": k, "command": c}))
            .collect::<Vec<_>>(),
    )
    .unwrap()
}

#[test]
fn key_bindings_are_added_after_the_users_and_not_again_once_removed() {
    let rig = Rig::new();
    std::fs::write(
        rig.user("keybindings.json"),
        "[\n  // mine\n  {\"key\": \"ctrl+j\", \"command\": \"a\"}\n]\n",
    )
    .unwrap();
    let plan = EditorDefaults {
        keybindings: Some(bindings(&[("ctrl+k", "b"), ("ctrl+j", "a")])),
        ..EditorDefaults::default()
    };
    rig.plan(&plan);
    apply(&rig.server);
    let text = std::fs::read_to_string(rig.user("keybindings.json")).unwrap();
    assert!(text.contains("// mine"), "{text}");
    let list = parse_merge_file(MergeFormat::Jsonc, text.as_bytes()).unwrap();
    assert_eq!(list.as_array().unwrap().len(), 2, "{text}");
    // Same plan again: no duplicate.
    apply(&rig.server);
    let again = std::fs::read_to_string(rig.user("keybindings.json")).unwrap();
    assert_eq!(again, text);
    // The user deletes Puddle's entry: it is not forced back.
    std::fs::write(
        rig.user("keybindings.json"),
        "[{\"key\": \"ctrl+j\", \"command\": \"a\"}]",
    )
    .unwrap();
    apply(&rig.server);
    let list: Value =
        serde_json::from_slice(&std::fs::read(rig.user("keybindings.json")).unwrap()).unwrap();
    assert_eq!(list.as_array().unwrap().len(), 1);
}

#[test]
fn an_entry_dropped_from_the_plan_leaves_a_strict_file() {
    let rig = Rig::new();
    rig.plan(&EditorDefaults {
        keybindings: Some(bindings(&[("ctrl+k", "b")])),
        ..EditorDefaults::default()
    });
    apply(&rig.server);
    assert!(rig.user("keybindings.json").exists());
    rig.unplan();
    apply(&rig.server);
    let list: Value =
        serde_json::from_slice(&std::fs::read(rig.user("keybindings.json")).unwrap()).unwrap();
    assert_eq!(list, json!([]));
}

#[test]
fn an_extension_is_installed_once_and_not_again_after_the_user_removed_it() {
    let rig = Rig::new();
    rig.plan(&EditorDefaults {
        extensions: vec!["Acme.Tool".to_owned(), "acme.there".to_owned()],
        ..EditorDefaults::default()
    });
    std::fs::create_dir(rig.dir.path().join("data/extensions/acme.there-1.0.0")).unwrap();
    let first = apply(&rig.server);
    assert_eq!(first.install, vec!["acme.tool".to_owned()]);
    record_installed(&rig.server, "acme.tool");
    // The user uninstalls it (the folder was never there in this test): no second install.
    let second = apply(&rig.server);
    assert!(second.install.is_empty(), "{second:?}");
}

#[test]
fn a_plan_file_that_is_not_valid_applies_nothing() {
    let rig = Rig::new();
    std::fs::write(rig.dir.path().join("plan.json"), "{ nope").unwrap();
    assert_eq!(apply(&rig.server), Outcome::default());
    assert!(!state_path(&rig.server).exists());
}

#[test]
fn a_settings_file_that_is_empty_or_holds_no_object_is_handled() {
    let rig = Rig::new();
    rig.plan(&dark(None));
    std::fs::write(rig.user("settings.json"), "  \n").unwrap();
    apply(&rig.server);
    assert_eq!(rig.settings()["workbench.colorTheme"], "Solarized Dark");
    let other = Rig::new();
    other.plan(&dark(None));
    std::fs::write(other.user("settings.json"), "[1]").unwrap();
    apply(&other.server);
    assert_eq!(
        std::fs::read_to_string(other.user("settings.json")).unwrap(),
        "[1]"
    );
}

#[test]
fn a_settings_file_with_a_repeated_key_is_left_as_it_is_for_a_set_and_for_a_removal() {
    let rig = Rig::new();
    let twice = r#"{"window.autoDetectColorScheme": false, "window.autoDetectColorScheme": true}"#;
    std::fs::write(rig.user("settings.json"), twice).unwrap();
    rig.plan(&dark(None));
    apply(&rig.server);
    // The seeded scheme is Puddle's to switch off, but a repeated key is no file to edit.
    assert_eq!(
        std::fs::read_to_string(rig.user("settings.json")).unwrap(),
        twice
    );

    let rig = Rig::new();
    rig.plan(&dark(None));
    apply(&rig.server);
    let mut settings = rig.settings();
    settings["unrelated"] = json!(1);
    std::fs::write(
        rig.user("settings.json"),
        format!(
            r#"{{"workbench.colorTheme": "Solarized Dark", "window.autoDetectColorScheme": false, "workbench.colorTheme": "Solarized Dark", "x": {}}}"#,
            settings["unrelated"]
        ),
    )
    .unwrap();
    let before = std::fs::read_to_string(rig.user("settings.json")).unwrap();
    rig.plan(&EditorDefaults::default());
    apply(&rig.server);
    assert_eq!(
        std::fs::read_to_string(rig.user("settings.json")).unwrap(),
        before
    );
}

#[test]
fn key_bindings_go_into_an_empty_list_and_after_a_trailing_comma() {
    let plan = EditorDefaults {
        keybindings: Some(bindings(&[("ctrl+k", "b")])),
        ..EditorDefaults::default()
    };
    for (before, count) in [
        ("[]", 1),
        ("[\n  {\"key\": \"ctrl+j\", \"command\": \"a\"},\n]", 2),
        ("  \n", 1),
    ] {
        let rig = Rig::new();
        std::fs::write(rig.user("keybindings.json"), before).unwrap();
        rig.plan(&plan);
        apply(&rig.server);
        let text = std::fs::read(rig.user("keybindings.json")).unwrap();
        let list = parse_merge_file(MergeFormat::Jsonc, &text).unwrap();
        assert_eq!(list.as_array().unwrap().len(), count, "{before:?}");
    }
}

#[test]
fn a_key_bindings_file_that_is_no_list_or_would_not_read_back_is_left_alone() {
    let plan = EditorDefaults {
        keybindings: Some(bindings(&[("ctrl+k", "b")])),
        ..EditorDefaults::default()
    };
    // A comment after the last entry swallows the comma the merge adds.
    for before in [
        "{\"a\": 1}",
        "[\n  {\"key\": \"ctrl+j\", \"command\": \"a\"} // mine\n]",
    ] {
        let rig = Rig::new();
        std::fs::write(rig.user("keybindings.json"), before).unwrap();
        rig.plan(&plan);
        apply(&rig.server);
        assert_eq!(
            std::fs::read_to_string(rig.user("keybindings.json")).unwrap(),
            before
        );
    }
}

#[test]
fn a_dropped_key_binding_stays_in_a_file_with_comments() {
    let rig = Rig::new();
    rig.plan(&EditorDefaults {
        keybindings: Some(bindings(&[("ctrl+k", "b")])),
        ..EditorDefaults::default()
    });
    apply(&rig.server);
    let mut text = std::fs::read_to_string(rig.user("keybindings.json")).unwrap();
    text.insert_str(0, "// my keys\n");
    std::fs::write(rig.user("keybindings.json"), &text).unwrap();
    rig.unplan();
    apply(&rig.server);
    assert_eq!(
        std::fs::read_to_string(rig.user("keybindings.json")).unwrap(),
        text
    );
}

#[cfg(unix)]
fn fake_installer(rig: &Rig, exit: i32) -> PathBuf {
    let program = rig.dir.path().join("code-server");
    let log = rig.dir.path().join("installed.log");
    puddle_test_kit::write_executable(
        &program,
        &format!(
            "#!/bin/sh\necho \"$2\" >> '{}'\nexit {exit}\n",
            log.display()
        ),
    );
    program
}

#[cfg(unix)]
#[tokio::test]
async fn the_missing_extensions_install_in_the_background_and_are_recorded_once_they_worked() {
    let rig = Rig::new();
    rig.plan(&EditorDefaults {
        extensions: vec!["acme.tool".to_owned(), "acme.other".to_owned()],
        ..EditorDefaults::default()
    });
    let program = fake_installer(&rig, 0);
    merge_at_start(&rig.server, &program, None).unwrap();
    let recorded = || read_applied(&state_path(&rig.server)).extensions;
    puddle_test_kit::eventually_async(
        "both installs are recorded",
        std::time::Duration::from_secs(30),
        || async {
            let ids = recorded();
            if ids.len() == 2 {
                Ok(())
            } else {
                Err(format!("{ids:?}"))
            }
        },
    )
    .await;
    let log = std::fs::read_to_string(rig.dir.path().join("installed.log")).unwrap();
    assert_eq!(log.lines().collect::<Vec<_>>(), ["acme.tool", "acme.other"]);
}

#[cfg(unix)]
#[tokio::test]
async fn an_install_that_fails_or_cannot_start_is_not_recorded() {
    let rig = Rig::new();
    let failing = fake_installer(&rig, 3);
    install_extensions(
        rig.server.clone(),
        vec!["acme.tool".to_owned()],
        failing,
        None,
    )
    .await;
    install_extensions(
        rig.server.clone(),
        vec!["acme.tool".to_owned()],
        rig.dir.path().join("missing"),
        None,
    )
    .await;
    assert_eq!(read_applied(&state_path(&rig.server)), Applied::default());
}

#[cfg(unix)]
#[test]
fn what_the_merge_made_is_handed_to_the_session_user() {
    use std::os::unix::fs::MetadataExt as _;
    let rig = Rig::new();
    rig.plan(&dark(None));
    let me = std::fs::metadata(rig.dir.path()).unwrap();
    let account = Account {
        name: "me".to_owned(),
        uid: me.uid(),
        gid: me.gid(),
        groups: vec![me.gid()],
        home: "/".to_owned(),
        shell: "/bin/sh".to_owned(),
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let _guard = runtime.enter();
    let program = rig.dir.path().join("no-such-server");
    merge_at_start(&rig.server, &program, Some(&account)).unwrap();
    assert_eq!(rig.settings()["workbench.colorTheme"], "Solarized Dark");
}

#[test]
fn the_same_plan_at_the_next_start_rewrites_nothing_and_a_folder_for_a_file_is_an_error_not_a_panic()
 {
    let rig = Rig::new();
    rig.plan(&dark(Some(15)));
    apply(&rig.server);
    let first = std::fs::read(rig.user("settings.json")).unwrap();
    let again = apply(&rig.server);
    assert_eq!(std::fs::read(rig.user("settings.json")).unwrap(), first);
    assert!(again.made.is_empty(), "{again:?}");

    let blocked = Rig::new();
    blocked.plan(&dark(None));
    std::fs::create_dir(blocked.user("settings.json")).unwrap();
    let done = apply(&blocked.server);
    assert!(done.made.is_empty(), "{done:?}");
}

#[test]
fn a_merge_that_writes_replaces_the_text_and_one_that_does_not_keeps_it() {
    assert_eq!(
        written(Merged::Write(b"new".to_vec()), b"old".to_vec()),
        b"new"
    );
    assert_eq!(written(Merged::Unchanged, b"old".to_vec()), b"old");
}

#[test]
fn a_size_taken_out_of_the_plan_is_removed_and_the_theme_stays() {
    let rig = Rig::new();
    rig.plan(&dark(Some(15)));
    apply(&rig.server);
    rig.plan(&dark(None));
    apply(&rig.server);
    let settings = rig.settings();
    assert!(settings.get("editor.fontSize").is_none(), "{settings}");
    assert_eq!(settings["workbench.colorTheme"], "Solarized Dark");
}

#[test]
fn a_removal_alone_edits_the_file_without_a_merge() {
    let out = edit_settings(
        Some(br#"{"a": 1, "b": 2}"#.to_vec()),
        &[],
        &[vec!["a".to_owned()]],
    )
    .expect("the file is editable")
    .expect("the key is gone");
    let text = String::from_utf8(out).expect("utf-8");
    assert!(!text.contains("\"a\""), "{text}");
    assert!(text.contains("\"b\": 2"), "{text}");
}

#[test]
fn a_refused_settings_merge_is_not_remembered_and_the_defaults_come_back_once_the_file_is_fixed() {
    let rig = Rig::new();
    let twice = r#"{"window.autoDetectColorScheme": false, "window.autoDetectColorScheme": true}"#;
    std::fs::write(rig.user("settings.json"), twice).unwrap();
    rig.plan(&dark(None));
    apply(&rig.server);
    assert_eq!(read_applied(&state_path(&rig.server)), Applied::default());
    std::fs::write(rig.user("settings.json"), "{}").unwrap();
    apply(&rig.server);
    let settings = rig.settings();
    assert_eq!(settings["workbench.colorTheme"], "Solarized Dark");
    assert_eq!(settings["window.autoDetectColorScheme"], false);
}

#[test]
fn a_settings_write_that_fails_is_not_remembered_and_is_tried_again() {
    use std::os::unix::fs::PermissionsExt;
    let rig = Rig::new();
    rig.plan(&dark(Some(14)));
    let folder = rig.user("");
    let open = std::fs::metadata(&folder).unwrap().permissions();
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o500)).unwrap();
    assert!(
        std::fs::write(rig.user("probe"), "").is_err(),
        "the folder must refuse writes for this test"
    );
    apply(&rig.server);
    assert_eq!(read_applied(&state_path(&rig.server)), Applied::default());
    std::fs::set_permissions(&folder, open).unwrap();
    apply(&rig.server);
    let settings = rig.settings();
    assert_eq!(settings["workbench.colorTheme"], "Solarized Dark");
    assert_eq!(settings["editor.fontSize"], 14);
}

#[test]
fn a_key_bindings_write_that_fails_is_not_remembered_and_is_tried_again() {
    use std::os::unix::fs::PermissionsExt;
    let rig = Rig::new();
    rig.plan(&EditorDefaults {
        keybindings: Some(bindings(&[("ctrl+k", "b")])),
        ..EditorDefaults::default()
    });
    let folder = rig.user("");
    let open = std::fs::metadata(&folder).unwrap().permissions();
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o500)).unwrap();
    assert!(
        std::fs::write(rig.user("probe"), "").is_err(),
        "the folder must refuse writes for this test"
    );
    apply(&rig.server);
    assert_eq!(
        read_applied(&state_path(&rig.server)).keybindings,
        Vec::<Value>::new()
    );
    std::fs::set_permissions(&folder, open).unwrap();
    apply(&rig.server);
    let list: Value =
        serde_json::from_slice(&std::fs::read(rig.user("keybindings.json")).unwrap()).unwrap();
    assert_eq!(list.as_array().unwrap().len(), 1);
}

#[test]
fn an_extension_whose_id_starts_another_installed_one_is_still_installed() {
    let rig = Rig::new();
    rig.plan(&EditorDefaults {
        extensions: vec!["acme.tool".to_owned(), "acme.other".to_owned()],
        ..EditorDefaults::default()
    });
    let extensions = rig.dir.path().join("data/extensions");
    std::fs::create_dir(extensions.join("acme.tool-extra-1.0.0")).unwrap();
    std::fs::create_dir(extensions.join("acme.other-2.1.0-linux-x64")).unwrap();
    let outcome = apply(&rig.server);
    assert_eq!(outcome.install, vec!["acme.tool".to_owned()]);
}

#[test]
fn a_folder_names_an_extension_only_as_id_dash_version() {
    assert!(is_folder_of("acme.tool-1.0.0", "acme.tool"));
    assert!(!is_folder_of("acme.tool-extra-1.0.0", "acme.tool"));
    assert!(!is_folder_of("acme.tool", "acme.tool"));
    assert!(!is_folder_of("acme.tool-", "acme.tool"));
    assert!(!is_folder_of("acme.tool1.0.0", "acme.tool"));
    assert!(!is_folder_of("other.acme.tool-1.0.0", "acme.tool"));
}

fn custom(text: &str) -> EditorDefaults {
    EditorDefaults {
        settings: Some(text.to_owned()),
        ..EditorDefaults::default()
    }
}

#[test]
fn custom_settings_are_merged_and_a_key_the_user_changed_stays_theirs() {
    let rig = Rig::new();
    std::fs::write(rig.user("settings.json"), "{\n  // mine\n  \"a.b\": 5\n}\n").unwrap();
    rig.plan(&custom(r#"{"a.b": 1, "c.d": "x"}"#));
    apply(&rig.server);
    let s = rig.settings();
    assert_eq!(s["c.d"], json!("x"));
    // `a.b` was never written by Puddle and the user has a value: it stays.
    assert_eq!(s["a.b"], json!(5));
    // A default that changes is followed while the user left the key alone.
    rig.plan(&custom(r#"{"a.b": 1, "c.d": "y"}"#));
    apply(&rig.server);
    assert_eq!(rig.settings()["c.d"], json!("y"));
    // The user edits `c.d`: the next change leaves it.
    let edited = std::fs::read_to_string(rig.user("settings.json"))
        .unwrap()
        .replace("\"y\"", "\"mine\"");
    std::fs::write(rig.user("settings.json"), edited).unwrap();
    rig.plan(&custom(r#"{"a.b": 1, "c.d": "z"}"#));
    apply(&rig.server);
    assert_eq!(rig.settings()["c.d"], json!("mine"));
}

#[test]
fn a_custom_key_that_is_no_longer_planned_is_handed_back_unless_changed() {
    let rig = Rig::new();
    rig.plan(&custom(r#"{"c.d": 1, "e.f": 2}"#));
    apply(&rig.server);
    let edited = std::fs::read_to_string(rig.user("settings.json"))
        .unwrap()
        .replace("\"e.f\": 2", "\"e.f\": 3");
    std::fs::write(rig.user("settings.json"), edited).unwrap();
    rig.plan(&custom("{}"));
    apply(&rig.server);
    let s = rig.settings();
    assert!(s.get("c.d").is_none());
    assert_eq!(s["e.f"], json!(3));
}

#[test]
fn a_custom_entry_for_an_owned_key_is_ignored() {
    let rig = Rig::new();
    rig.plan(&custom(r#"{"editor.fontSize": 30}"#));
    apply(&rig.server);
    assert!(!rig.user("settings.json").exists() || rig.settings().get("editor.fontSize").is_none());
}
