// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn empty_layer_serialises_to_an_empty_object() {
    assert_eq!(
        serde_json::to_string(&WorkspaceLayer::default()).unwrap(),
        "{}"
    );
    assert!(WorkspaceLayer::default().is_empty());
}

#[test]
fn set_values_use_the_documented_names() {
    let l = WorkspaceLayer {
        memory: Some(MemoryMib::new(1024).unwrap()),
        cpus: Some(CpuCount::new(2).unwrap()),
        local_toggles: LocalToggles {
            metadata: Some(false),
            ..LocalToggles::default()
        },
        wildcards_reach_local: Some(true),
        reconnection_grace: Some(ReconnectionGrace::new(60).unwrap()),
        zoom_hotkeys: Some(false),
        clipboard_read: Some(ClipboardRead::Deny),
        direct_ssh: Some(true),
        capture_logins: Some(false),
        warn_foreign_credentials: Some(false),
        extra: BTreeMap::new(),
    };
    assert_eq!(
        serde_json::to_string(&l).unwrap(),
        r#"{"memory":1024,"cpus":2,"local_toggles":{"metadata":false},"wildcards_reach_local":true,"reconnection_grace":60,"zoom_hotkeys":false,"clipboard_read":"deny","direct_ssh":true,"capture_logins":false,"warn_foreign_credentials":false}"#
    );
    assert_eq!(
        serde_json::from_str::<WorkspaceLayer>(&serde_json::to_string(&l).unwrap()).unwrap(),
        l
    );
}

#[test]
fn null_means_unset() {
    let l: WorkspaceLayer =
        serde_json::from_str(r#"{"memory":null,"local_toggles":{"private":null}}"#).unwrap();
    assert!(l.is_empty());
}

#[test]
fn invalid_values_are_rejected_through_the_flattened_map() {
    assert!(serde_json::from_str::<WorkspaceLayer>(r#"{"memory":1}"#).is_err());
    assert!(serde_json::from_str::<WorkspaceLayer>(r#"{"memory":"8192"}"#).is_err());
    assert!(serde_json::from_str::<WorkspaceLayer>(r#"{"cpus":0}"#).is_err());
    assert!(serde_json::from_str::<WorkspaceLayer>(r#"{"cpus":33}"#).is_err());
    assert!(
        serde_json::from_str::<WorkspaceLayer>(r#"{"local_toggles":{"private":"yes"}}"#).is_err()
    );
    assert!(serde_json::from_str::<WorkspaceLayer>(r#"{"clipboard_read":"maybe"}"#).is_err());
    assert!(serde_json::from_str::<WorkspaceLayer>(r#"{"reconnection_grace":-1}"#).is_err());
}

#[test]
fn unknown_fields_are_kept_and_listed() {
    let l: WorkspaceLayer = serde_json::from_str(
        r#"{"swap":4,"local_toggles":{"private":true,"vpn":true},"memory":2048}"#,
    )
    .unwrap();
    assert_eq!(l.memory.unwrap().get(), 2048);
    assert_eq!(l.local_toggles.private, Some(true));
    assert!(!l.is_empty());
    let mut unknown = Vec::new();
    l.collect_unknown("workspace_defaults.", &mut unknown);
    assert_eq!(
        unknown,
        [
            "workspace_defaults.swap",
            "workspace_defaults.local_toggles.vpn"
        ]
    );
    let back = serde_json::to_value(&l).unwrap();
    assert_eq!(back["swap"], 4);
    assert_eq!(back["local_toggles"]["vpn"], true);
}

#[test]
fn every_category_has_its_field() {
    for category in LocalCategory::ALL {
        let json = format!(r#"{{"{}":true}}"#, category.key());
        let t: LocalToggles = serde_json::from_str(&json).unwrap();
        assert!(t.extra.is_empty(), "{category}: no field named {json}");
        for other in LocalCategory::ALL {
            assert_eq!(t.get(other), (other == category).then_some(true), "{other}");
        }
        assert_eq!(serde_json::to_string(&t).unwrap(), json);
    }
}

#[test]
fn a_layer_with_only_unknown_toggles_is_not_empty() {
    let l: WorkspaceLayer = serde_json::from_str(r#"{"local_toggles":{"vpn":true}}"#).unwrap();
    assert!(!l.local_toggles.is_empty());
    assert_eq!(
        serde_json::to_string(&l).unwrap(),
        r#"{"local_toggles":{"vpn":true}}"#
    );
}
