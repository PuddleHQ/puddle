// SPDX-License-Identifier: AGPL-3.0-or-later
use serde_json::json;

use super::*;
use crate::{Consent, UnixMillis};

#[test]
fn empty_object_is_all_defaults_and_writes_back_as_version_only() {
    let loaded = GlobalSettings::from_document(json!({})).unwrap();
    assert_eq!(loaded.settings, GlobalSettings::default());
    assert_eq!(loaded.migrated_from, Some(1), "no version means version 1");
    assert_eq!(
        loaded.settings.to_document(),
        json!({ "schema_version": 2 })
    );
}

#[test]
fn the_ssh_agent_pipe_is_unset_by_default_and_a_blank_value_is_unset() {
    assert_eq!(UiPrefs::default().ssh_agent_pipe(), None);
    let named = GlobalSettings::from_document(
        json!({"ui": {"ssh_agent_pipe": r"  \\.\pipe\pageant.alice  "}}),
    )
    .unwrap();
    assert_eq!(
        named.settings.ui.ssh_agent_pipe(),
        Some(r"\\.\pipe\pageant.alice")
    );
    assert_eq!(
        named.settings.to_document()["ui"],
        json!({"ssh_agent_pipe": r"  \\.\pipe\pageant.alice  "}),
        "the stored text is kept as written"
    );
    let blank = GlobalSettings::from_document(json!({"ui": {"ssh_agent_pipe": "  "}})).unwrap();
    assert_eq!(blank.settings.ui.ssh_agent_pipe(), None);
}

#[test]
fn the_free_space_lines_default_to_20_and_5_gb_and_a_stored_pair_out_of_order_is_clamped() {
    let defaults = UiPrefs::default();
    assert!(defaults.notify_low_disk_space());
    let limits = defaults.disk_limits();
    assert_eq!((limits.warn().gb(), limits.fail().gb()), (20, 5));

    let loaded = GlobalSettings::from_document(json!({
        "ui": {"notify_low_disk_space": false, "disk_warn_free_gb": 50, "disk_fail_free_gb": 10}
    }))
    .unwrap();
    let ui = &loaded.settings.ui;
    assert!(!ui.notify_low_disk_space());
    let limits = ui.disk_limits();
    assert_eq!((limits.warn().gb(), limits.fail().gb()), (50, 10));
    assert_eq!(
        loaded.settings.to_document()["ui"],
        json!({"notify_low_disk_space": false, "disk_warn_free_gb": 50, "disk_fail_free_gb": 10})
    );

    let swapped = GlobalSettings::from_document(
        json!({"ui": {"disk_warn_free_gb": 8, "disk_fail_free_gb": 30}}),
    )
    .unwrap();
    let limits = swapped.settings.ui.disk_limits();
    assert_eq!((limits.warn().gb(), limits.fail().gb()), (8, 8));

    let zero = GlobalSettings::from_document(json!({"ui": {"disk_warn_free_gb": 0}}));
    assert!(zero.is_err(), "a line of 0 GB would never warn");
}

#[test]
fn vscode_server_defaults() {
    let v = VsCodeServer::default();
    assert!(!v.telemetry());
    assert!(v.auto_update());
    let loaded = GlobalSettings::from_document(
        json!({"vscode_server":{"telemetry":true,"auto_update":false}}),
    )
    .unwrap();
    assert!(loaded.settings.vscode_server.telemetry());
    assert!(!loaded.settings.vscode_server.auto_update());
}

#[test]
fn ui_prefs_and_server_choice_default_and_round_trip() {
    let loaded = GlobalSettings::from_document(json!({})).unwrap();
    assert_eq!(
        loaded.settings.vscode_server.server(),
        ServerChoice::CodeServer
    );
    let ui = &loaded.settings.ui;
    assert_eq!(ui.theme(), ThemeChoice::System);
    assert_eq!(ui.density(), DensityChoice::Comfortable);
    assert_eq!(ui.palette(), PaletteChoice::Pond);
    assert_eq!(ui.custom_accent, None);
    assert!(ui.notifications());
    assert!(!ui.sound());
    assert_eq!(ui.close_behaviour(), CloseBehaviour::Tray);
    assert!(ui.notify_only_unfocused());
    assert!(!ui.minimize_to_tray());
    assert!(ui.badge());
    assert!(ui.notify_oom());
    assert!(ui.notify_unexpected_stop());
    assert!(ui.confirm_global_rules());
    assert!(!ui.inbox_shortcuts());
    assert!(!ui.start_at_login());
    assert_eq!(ui.app_icon(), AppIconChoice::Duck);

    let doc = json!({
        "schema_version": 2,
        "vscode_server": { "server": "microsoft" },
        "ui": { "theme": "dark", "density": "compact", "palette": "custom", "custom_accent": "#0a6a85", "notifications": false, "sound": true, "close_behaviour": "quit", "minimize_to_tray": true,
                 "notify_only_unfocused": false, "badge": false, "notify_oom": false,
                 "notify_unexpected_stop": false, "confirm_global_rules": false, "inbox_shortcuts": true,
                 "start_at_login": true, "app_icon": "duck_night" },
    });
    let loaded = GlobalSettings::from_document(doc.clone()).unwrap();
    assert_eq!(loaded.unknown_fields, Vec::<String>::new());
    assert_eq!(
        loaded.settings.vscode_server.server(),
        ServerChoice::Microsoft
    );
    assert_eq!(loaded.settings.ui.theme(), ThemeChoice::Dark);
    assert_eq!(loaded.settings.ui.density(), DensityChoice::Compact);
    assert_eq!(loaded.settings.ui.palette(), PaletteChoice::Custom);
    assert_eq!(
        loaded
            .settings
            .ui
            .custom_accent
            .as_ref()
            .map(HexColour::as_str),
        Some("#0a6a85")
    );
    assert!(!loaded.settings.ui.notifications());
    assert!(loaded.settings.ui.sound());
    assert_eq!(loaded.settings.ui.close_behaviour(), CloseBehaviour::Quit);
    let ui = &loaded.settings.ui;
    assert!(ui.minimize_to_tray());
    assert!(!ui.notify_only_unfocused());
    assert!(!ui.badge());
    assert!(!ui.notify_oom());
    assert!(!ui.notify_unexpected_stop());
    assert!(!ui.confirm_global_rules());
    assert!(ui.inbox_shortcuts());
    assert!(ui.start_at_login());
    assert_eq!(ui.app_icon(), AppIconChoice::DuckNight);
    assert_eq!(loaded.settings.to_document(), doc);
}

#[test]
fn a_document_from_before_these_fields_loads_unchanged_and_ui_unknowns_are_kept() {
    let old = json!({ "schema_version": 2, "vscode_server": { "telemetry": true } });
    let loaded = GlobalSettings::from_document(old.clone()).unwrap();
    assert_eq!(loaded.migrated_from, None);
    assert_eq!(loaded.settings.to_document(), old);

    let newer = json!({ "schema_version": 2, "ui": { "font_scale": 2, "theme": "light" } });
    let loaded = GlobalSettings::from_document(newer.clone()).unwrap();
    assert_eq!(loaded.unknown_fields, ["ui.font_scale"]);
    assert_eq!(loaded.settings.to_document(), newer);
}

#[test]
fn an_invalid_choice_fails_the_document() {
    let err = GlobalSettings::from_document(json!({"ui":{"theme":"purple"}})).unwrap_err();
    assert!(matches!(err, SettingsError::Invalid { kind: "global", .. }));
}

#[test]
fn unknown_fields_at_every_level_are_listed_and_written_back() {
    let doc = json!({
        "schema_version": 2,
        "rule_sets": ["trackers"],
        "workspace_defaults": { "swap": 2, "local_toggles": { "vpn": true } },
        "vscode_server": { "channel": "insiders" },
        "consents": {
            "usage_survey": { "state": "granted" },
            "telemetry": { "state": "not_asked" }
        }
    });
    let loaded = GlobalSettings::from_document(doc.clone()).unwrap();
    assert_eq!(
        loaded.unknown_fields,
        [
            "rule_sets",
            "workspace_defaults.swap",
            "workspace_defaults.local_toggles.vpn",
            "vscode_server.channel",
            "consents.usage_survey",
        ]
    );
    let mut want = doc;
    // A not-asked consent is the default, so it isn't written out.
    want["consents"]
        .as_object_mut()
        .unwrap()
        .remove("telemetry");
    assert_eq!(loaded.settings.to_document(), want);
}

#[test]
fn a_version_1_document_keeps_its_defaults_under_the_new_name() {
    let old = json!({
        "schema_version": 1,
        "sandbox_defaults": { "memory": 4096, "local_toggles": { "private": true } },
        "ui": { "theme": "dark" },
    });
    let loaded = GlobalSettings::from_document(old).unwrap();
    assert_eq!(loaded.migrated_from, Some(1));
    assert_eq!(loaded.unknown_fields, Vec::<String>::new());
    assert_eq!(
        loaded.settings.workspace_defaults.memory.unwrap().get(),
        4096
    );
    let doc = loaded.settings.to_document();
    assert_eq!(doc["schema_version"], 2);
    assert_eq!(doc["workspace_defaults"]["memory"], 4096);
    assert_eq!(doc["ui"]["theme"], "dark");
    assert!(doc.get("sandbox_defaults").is_none(), "{doc}");
    // Written back, it loads as it is.
    assert_eq!(
        GlobalSettings::from_document(doc).unwrap().migrated_from,
        None
    );
}

#[test]
fn a_document_with_both_names_keeps_the_new_one_and_the_old_as_unknown() {
    let both = json!({
        "schema_version": 1,
        "sandbox_defaults": { "memory": 1024 },
        "workspace_defaults": { "memory": 4096 },
    });
    let loaded = GlobalSettings::from_document(both).unwrap();
    assert_eq!(
        loaded.settings.workspace_defaults.memory.unwrap().get(),
        4096
    );
    assert_eq!(loaded.unknown_fields, ["sandbox_defaults"]);
}

#[test]
fn errors_name_the_document_kind() {
    let err =
        GlobalSettings::from_document(json!({"workspace_defaults":{"memory":1}})).unwrap_err();
    assert!(matches!(err, SettingsError::Invalid { kind: "global", .. }));
    assert!(err.to_string().contains("invalid memory size"), "{err}");
    let err = GlobalSettings::from_document(json!({"schema_version":3})).unwrap_err();
    assert!(matches!(
        err,
        SettingsError::NewerSchema {
            kind: "global",
            found: 3,
            supported: 2
        }
    ));
}

#[test]
fn first_run_defaults_to_not_completed_and_round_trips() {
    let loaded = GlobalSettings::from_document(json!({})).unwrap();
    assert!(!loaded.settings.first_run.is_completed());
    assert_eq!(
        loaded.settings.to_document(),
        json!({ "schema_version": 2 }),
        "an unset flow is not written"
    );

    let doc =
        json!({ "schema_version": 2, "first_run": { "completed_at": 1_700_000_000_000_u64 } });
    let loaded = GlobalSettings::from_document(doc.clone()).unwrap();
    assert!(loaded.settings.first_run.is_completed());
    assert_eq!(
        loaded.settings.first_run.completed_at,
        Some(UnixMillis(1_700_000_000_000))
    );
    assert_eq!(loaded.settings.to_document(), doc);
}

#[test]
fn first_run_keeps_fields_it_does_not_know() {
    let doc = json!({ "schema_version": 2, "first_run": { "completed_at": 5, "tour": "v2" } });
    let loaded = GlobalSettings::from_document(doc.clone()).unwrap();
    assert_eq!(loaded.unknown_fields, ["first_run.tour"]);
    assert_eq!(loaded.settings.to_document(), doc);
}

#[test]
fn consents_round_trip() {
    let mut g = GlobalSettings::default();
    g.consents.set(
        ConsentKind::VsCodeServer,
        Consent::Granted {
            at: UnixMillis(42),
            terms_version: TermsVersion::new("https://example.test/terms").unwrap(),
        },
    );
    let doc = g.to_document();
    assert_eq!(doc["consents"]["vscode_server"]["state"], "granted");
    assert_eq!(GlobalSettings::from_document(doc).unwrap().settings, g);
}

fn chosen_with(consent: Consent) -> GlobalSettings {
    let mut g = GlobalSettings::default();
    g.vscode_server.server = Some(ServerChoice::Microsoft);
    g.consents.set(ConsentKind::VsCodeServer, consent);
    g
}

fn granted(terms: &str) -> Consent {
    Consent::Granted {
        at: UnixMillis(1),
        terms_version: TermsVersion::new(terms).unwrap(),
    }
}

#[test]
fn microsoft_runs_only_when_chosen_and_consent_covers_todays_terms() {
    assert_eq!(
        chosen_with(granted(MICROSOFT_TERMS_VERSION)).editor_server(),
        ServerChoice::Microsoft
    );
    // The choice alone is not enough, whatever the record says instead of a grant.
    for consent in [
        Consent::NotAsked,
        Consent::Declined {
            at: UnixMillis(1),
            terms_version: TermsVersion::new(MICROSOFT_TERMS_VERSION).unwrap(),
        },
        granted("https://code.visualstudio.com/license/server-2027"),
    ] {
        let g = chosen_with(consent.clone());
        assert_eq!(g.editor_server(), ServerChoice::CodeServer, "{consent:?}");
        assert!(!g.microsoft_consented(), "{consent:?}");
    }
    // A consent with the choice left on code-server stays code-server.
    let mut g = chosen_with(granted(MICROSOFT_TERMS_VERSION));
    g.vscode_server.server = None;
    assert!(g.microsoft_consented());
    assert_eq!(g.editor_server(), ServerChoice::CodeServer);
}

#[test]
fn the_settings_screen_shows_the_terms_version_the_gate_checks() {
    // The popup records the version it shows; a different string there would record consents
    // the gate never accepts.
    let model = include_str!("../../../../ui/src/lib/settings/model.ts");
    assert!(
        model.contains(&format!("MS_TERMS_URL = \"{MICROSOFT_TERMS_VERSION}\"")),
        "ui/src/lib/settings/model.ts shows other terms than MICROSOFT_TERMS_VERSION"
    );
}

#[test]
fn the_global_mise_switch_defaults_on_and_round_trips() {
    assert!(GlobalSettings::default().tools.mise());
    assert_eq!(GlobalSettings::default().to_document().get("tools"), None);
    let loaded =
        GlobalSettings::from_document(json!({"tools": {"mise": false, "later": 2}})).unwrap();
    assert!(!loaded.settings.tools.mise());
    assert_eq!(loaded.unknown_fields, ["tools.later"]);
    assert_eq!(loaded.settings.to_document()["tools"]["mise"], false);
}
