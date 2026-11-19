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
