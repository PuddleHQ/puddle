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
        loaded.settings.to_document().unwrap()["dangerous"]["reach_puddle_endpoints"],
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
        WorkspaceSettings::default().to_document().unwrap(),
        json!({"schema_version": 1})
    );
}
