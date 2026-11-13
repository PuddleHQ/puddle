// SPDX-License-Identifier: GPL-3.0-or-later
use serde_json::json;

use super::*;

#[test]
fn unknown_fields_are_listed_with_their_path() {
    let loaded = WorkspaceSettings::from_document(json!({
        "dangerous": { "reach_puddle_endpoints": true },
        "overrides": { "local_toggles": { "vpn": false } }
    }))
    .unwrap();
    assert_eq!(
        loaded.unknown_fields,
        ["dangerous", "overrides.local_toggles.vpn"]
    );
    assert_eq!(
        loaded.settings.to_document()["dangerous"]["reach_puddle_endpoints"],
        true
    );
}

#[test]
fn errors_name_the_document_kind() {
    let err = WorkspaceSettings::from_document(json!([])).unwrap_err();
    assert_eq!(err, SettingsError::NotAnObject { kind: "workspace" });
    let err =
        WorkspaceSettings::from_document(json!({"overrides":{"zoom_hotkeys":1}})).unwrap_err();
    assert!(
        matches!(
            err,
            SettingsError::Invalid {
                kind: "workspace",
                ..
            }
        ),
        "{err}"
    );
}

#[test]
fn a_document_without_overrides_is_version_only() {
    assert_eq!(
        WorkspaceSettings::default().to_document(),
        json!({"schema_version": 1})
    );
}

#[test]
fn tool_switches_round_trip_and_unknown_ones_are_kept() {
    let loaded = WorkspaceSettings::from_document(json!({
        "tools": { "mise": false, "repo_code": true, "later": 1 }
    }))
    .unwrap();
    assert_eq!(loaded.unknown_fields, ["tools.later"]);
    assert_eq!(loaded.settings.tools.mise, Some(false));
    assert!(loaded.settings.tools.repo_code());
    assert!(!loaded.settings.tools.mise());
    assert_eq!(loaded.settings.to_document()["tools"]["later"], 1);
}

#[test]
fn mise_mode_answers() {
    use crate::GlobalSettings;
    let mut global = GlobalSettings::default();
    let mut ws = WorkspaceSettings::default();
    assert_eq!(mise_mode(&global, None), MiseMode::On { trust: true });
    ws.tools.repo_code = Some(false);
    assert_eq!(mise_mode(&global, Some(&ws)), MiseMode::On { trust: false });
    ws.tools.mise = Some(false);
    assert_eq!(mise_mode(&global, Some(&ws)), MiseMode::Off);
    ws.tools.mise = Some(true);
    global.tools.mise = Some(false);
    assert_eq!(mise_mode(&global, Some(&ws)), MiseMode::Off);
}
