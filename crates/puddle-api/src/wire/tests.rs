// SPDX-License-Identifier: GPL-3.0-or-later
#[test]
fn every_reason_a_rule_was_deleted_reaches_the_wire() {
    for (from, to) in [
        (store::RuleDeleteReason::User, RuleDeleteReason::User),
        (
            store::RuleDeleteReason::WorkspaceDeleted,
            RuleDeleteReason::WorkspaceDeleted,
        ),
        (
            store::RuleDeleteReason::SetDeleted,
            RuleDeleteReason::SetDeleted,
        ),
    ] {
        assert_eq!(RuleDeleteReason::from(from), to);
    }
}

#[test]
fn every_connection_outcome_reaches_the_wire_under_its_own_name() {
    use puddle_types::ConnectionOutcome as T;
    for (from, to, name) in [
        (T::Connected, ConnectionOutcome::Connected, "connected"),
        (
            T::UpstreamRefused,
            ConnectionOutcome::UpstreamRefused,
            "upstream_refused",
        ),
        (
            T::ConnectFailed,
            ConnectionOutcome::ConnectFailed,
            "connect_failed",
        ),
        (T::Timeout, ConnectionOutcome::Timeout, "timeout"),
    ] {
        let wire = ConnectionOutcome::from(from);
        assert_eq!(wire, to);
        assert_eq!(serde_json::to_value(wire).unwrap(), name);
        assert_eq!(from.as_str(), name);
    }
}

use std::collections::BTreeSet;

use puddle_types::{Host, PendingId, RuleId};
use serde_json::json;

use super::*;
use serde_json::Value;

fn keys(v: &Value) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    collect(v, "", &mut out);
    out
}

fn collect(v: &Value, prefix: &str, out: &mut BTreeSet<String>) {
    if let Value::Object(map) = v {
        for (k, child) in map {
            let path = format!("{prefix}{k}");
            if child.is_object() {
                collect(child, &format!("{path}."), out);
            } else {
                out.insert(path);
            }
        }
    }
}

/// A storage layer with every known field set, built from JSON so a new storage field shows
/// up here as an unknown field and fails the key comparison below.
fn full_layer() -> settings::WorkspaceLayer {
    let doc = json!({
        "schema_version": 1,
        "overrides": {
            "memory": 1024,
            "cpus": 2,
            "local_toggles": {
                "loopback": true, "private": false, "link_local": true,
                "metadata": false, "special": true, "company_network": true
            },
            "wildcards_reach_local": true,
            "reconnection_grace": 60,
            "zoom_hotkeys": false,
            "clipboard_read": "deny",
            "direct_ssh": true,
            "capture_logins": false,
            "warn_foreign_credentials": false
        }
    });
    let loaded = settings::WorkspaceSettings::from_document(doc).unwrap();
    assert_eq!(loaded.unknown_fields, Vec::<String>::new());
    loaded.settings.overrides
}

#[test]
fn settings_layer_has_exactly_the_storage_fields() {
    let layer = full_layer();
    let storage = serde_json::to_value(&layer).unwrap();
    let wire = serde_json::to_value(SettingsLayer::from(&layer)).unwrap();
    assert_eq!(keys(&storage), keys(&wire));
    assert_eq!(
        storage, wire,
        "same names and values when every field is set"
    );
}

#[test]
fn settings_layer_round_trips_and_keeps_unknown_stored_fields() {
    let mut stored = settings::WorkspaceSettings::from_document(
        json!({"overrides": {"swap": 4, "memory": 2048}}),
    )
    .unwrap()
    .settings;
    let wire = SettingsLayer::from(&full_layer());
    wire.clone().apply_to(&mut stored.overrides).unwrap();
    assert_eq!(SettingsLayer::from(&stored.overrides), wire);
    assert_eq!(stored.to_document()["overrides"]["swap"], 4);
    // Clearing every value leaves only the unknown field.
    SettingsLayer::default()
        .apply_to(&mut stored.overrides)
        .unwrap();
    assert_eq!(
        stored.to_document(),
        json!({"schema_version": 1, "overrides": {"swap": 4}})
    );
}

#[test]
fn out_of_range_settings_are_refused() {
    let mut layer = settings::WorkspaceLayer::default();
    for bad in [
        SettingsLayer {
            memory: Some(1),
            ..SettingsLayer::default()
        },
        SettingsLayer {
            cpus: Some(0),
            ..SettingsLayer::default()
        },
        SettingsLayer {
            cpus: Some(33),
            ..SettingsLayer::default()
        },
        SettingsLayer {
            reconnection_grace: Some(1),
            ..SettingsLayer::default()
        },
    ] {
        let err = bad.apply_to(&mut layer).unwrap_err();
        assert_eq!(err.status(), axum::http::StatusCode::UNPROCESSABLE_ENTITY);
    }
}

#[test]
fn palette_choices_convert_both_ways() {
    for (wire, stored) in [
        (PaletteChoice::Pond, settings::PaletteChoice::Pond),
        (PaletteChoice::Dusk, settings::PaletteChoice::Dusk),
        (PaletteChoice::Meadow, settings::PaletteChoice::Meadow),
        (PaletteChoice::Ember, settings::PaletteChoice::Ember),
        (PaletteChoice::Graphite, settings::PaletteChoice::Graphite),
        (PaletteChoice::Custom, settings::PaletteChoice::Custom),
    ] {
        assert_eq!(settings::PaletteChoice::from(wire), stored);
        assert_eq!(PaletteChoice::from(stored), wire);
    }
}

#[test]
fn a_custom_colour_that_is_not_hex_is_refused() {
    let mut prefs = settings::UiPrefs::default();
    let bad = UiPrefs {
        custom_text: Some("teal".to_owned()),
        ..UiPrefs::default()
    };
    let err = bad.apply_to(&mut prefs).unwrap_err();
    assert_eq!(err.status(), axum::http::StatusCode::UNPROCESSABLE_ENTITY);
}

#[test]
fn the_free_space_lines_are_validated_alone_and_together() {
    let apply = |warn, fail| {
        let mut prefs = settings::UiPrefs::default();
        UiPrefs {
            disk_warn_free_gb: warn,
            disk_fail_free_gb: fail,
            ..UiPrefs::default()
        }
        .apply_to(&mut prefs)
        .map(|()| prefs.disk_limits())
    };
    let lines = apply(Some(30), Some(30)).unwrap();
    assert_eq!((lines.warn().gb(), lines.fail().gb()), (30, 30));
    let defaults = apply(None, None).unwrap();
    assert_eq!((defaults.warn().gb(), defaults.fail().gb()), (20, 5));
    for bad in [
        apply(Some(0), None),
        apply(None, Some(10_001)),
        apply(Some(10), Some(11)),
        // The failure line is compared with the warning line that applies: the default 20.
        apply(None, Some(21)),
        // And a warning line under the default failure line of 5.
        apply(Some(4), None),
    ] {
        let err = bad.unwrap_err();
        assert_eq!(err.status(), axum::http::StatusCode::UNPROCESSABLE_ENTITY);
    }
}

#[test]
fn density_choices_convert_both_ways() {
    for (wire, stored) in [
        (
            DensityChoice::Comfortable,
            settings::DensityChoice::Comfortable,
        ),
        (DensityChoice::Compact, settings::DensityChoice::Compact),
    ] {
        assert_eq!(settings::DensityChoice::from(wire), stored);
        assert_eq!(DensityChoice::from(stored), wire);
    }
}

#[test]
fn global_request_sets_vscode_options() {
    let mut g = settings::GlobalSettings::default();
    GlobalSettingsRequest {
        workspace_defaults: SettingsLayer {
            zoom_hotkeys: Some(false),
            ..SettingsLayer::default()
        },
        vscode_server: VsCodeServer {
            server: Some(ServerChoice::Microsoft),
            telemetry: Some(true),
            auto_update: Some(false),
        },
        ui: UiPrefs {
            theme: Some(ThemeChoice::Dark),
            density: Some(DensityChoice::Compact),
            palette: Some(PaletteChoice::Custom),
            custom_accent: Some("#0A6A85".to_owned()),
            custom_background: None,
            custom_surface: None,
            custom_text: Some("#102030".to_owned()),
            notifications: Some(false),
            sound: Some(true),
            close_behaviour: Some(CloseBehaviour::Quit),
            minimize_to_tray: Some(true),
            app_icon: Some(AppIconChoice::DuckSun),
            notify_only_unfocused: Some(false),
            badge: Some(false),
            notify_oom: Some(false),
            notify_disk_full: Some(false),
            disk_warning_percent: Some(90),
            notify_unexpected_stop: Some(false),
            confirm_global_rules: Some(false),
            inbox_shortcuts: Some(true),
            start_at_login: Some(true),
            notify_low_disk_space: Some(false),
            disk_warn_free_gb: Some(50),
            disk_fail_free_gb: Some(10),
        },
        tools: GlobalTools { mise: Some(false) },
    }
    .apply_to(&mut g)
    .unwrap();
    assert_eq!(g.vscode_server.server(), settings::ServerChoice::Microsoft);
    assert_eq!(g.ui.theme(), settings::ThemeChoice::Dark);
    assert_eq!(g.ui.density(), settings::DensityChoice::Compact);
    assert_eq!(g.ui.palette(), settings::PaletteChoice::Custom);
    assert_eq!(
        colour_text(g.ui.custom_accent.as_ref()).as_deref(),
        Some("#0a6a85")
    );
    assert_eq!(colour_text(g.ui.custom_background.as_ref()), None);
    assert!(!g.ui.notifications());
    assert!(g.ui.sound());
    assert_eq!(g.ui.close_behaviour(), settings::CloseBehaviour::Quit);
    assert!(g.ui.minimize_to_tray());
    assert_eq!(g.ui.app_icon(), settings::AppIconChoice::DuckSun);
    assert!(!g.ui.notify_only_unfocused());
    assert!(!g.ui.badge());
    assert!(!g.ui.notify_oom());
    assert!(!g.ui.notify_disk_full());
    assert_eq!(g.ui.disk_warning_percent().get(), 90);
    assert!(!g.ui.notify_unexpected_stop());
    assert!(!g.ui.confirm_global_rules());
    assert!(g.ui.inbox_shortcuts());
    assert!(g.ui.start_at_login());
    assert!(!g.ui.notify_low_disk_space());
    let lines = g.ui.disk_limits();
    assert_eq!((lines.warn().gb(), lines.fail().gb()), (50, 10));
    assert!(g.vscode_server.telemetry());
    assert!(!g.vscode_server.auto_update());
    assert!(!g.tools.mise());
    let view = GlobalSettingsView::new(&settings::Loaded {
        settings: g,
        migrated_from: None,
        unknown_fields: vec![],
    });
    assert_eq!(view.effective.zoom_hotkeys.source, SettingSource::Global);
    assert!(!view.effective.zoom_hotkeys.value);
    assert_eq!(view.effective.memory.source, SettingSource::Default);
}

#[test]
fn decision_request_defaults_are_narrowest() {
    let r = DecisionRequest::default()
        .into_resolution(Effect::Allow)
        .unwrap();
    assert_eq!(r, store::Resolution::allow());
    let r = DecisionRequest {
        scope: ScopeChoice::Global,
        rule_set: None,
        suffix: Some("example.com".into()),
        expires_in_secs: Some(60),
    }
    .into_resolution(Effect::Deny)
    .unwrap();
    assert_eq!(r.effect, store::Effect::Deny);
    assert_eq!(r.scope, store::ScopeChoice::Global);
    assert_eq!(
        r.pattern,
        store::PatternChoice::Suffix("example.com".into())
    );
    assert_eq!(r.expires_in, Some(Duration::from_secs(60)));
    let parsed: DecisionRequest = serde_json::from_str("{}").unwrap();
    assert_eq!(parsed, DecisionRequest::default());
    assert!(serde_json::from_str::<DecisionRequest>(r#"{"scope":"everything"}"#).is_err());
    assert!(serde_json::from_str::<DecisionRequest>(r#"{"extra":1}"#).is_err());
}

#[test]
fn a_decision_into_a_rule_set_needs_the_default_scope() {
    let parsed: DecisionRequest = serde_json::from_str(r#"{"rule_set":4}"#).unwrap();
    assert_eq!(
        parsed.into_resolution(Effect::Allow).unwrap().scope,
        store::ScopeChoice::Set(4)
    );
    let both: DecisionRequest = serde_json::from_str(r#"{"rule_set":4,"scope":"global"}"#).unwrap();
    assert!(both.into_resolution(Effect::Allow).is_err());
}

#[test]
fn set_scopes_map_both_ways() {
    let wire = RuleScope::from_store(store::Scope::Set(3)).unwrap();
    assert_eq!(wire, RuleScope::Set { set: 3 });
    assert_eq!(
        serde_json::to_value(&wire).unwrap(),
        serde_json::json!({"type": "set", "set": 3})
    );
    assert_eq!(store::Scope::from(wire), store::Scope::Set(3));
}

#[test]
fn rules_map_with_their_scope_and_pattern_kind() {
    let workspace = WorkspaceId::new("box").unwrap();
    let rule = store::Rule {
        id: RuleId(3),
        user: store::UserId::OWNER,
        scope: store::Scope::Workspace(workspace.clone()),
        pattern: store::Pattern::parse("*.example.com").unwrap(),
        effect: store::Effect::Deny,
        expires_at: None,
        created_at: 5,
        created_by: store::Actor::Ui,
        source_pending_id: Some(PendingId(9)),
    };
    let wire = Rule::from_store(rule).unwrap();
    assert_eq!(
        serde_json::to_value(&wire).unwrap(),
        json!({
            "id": 3, "scope": {"type": "workspace", "workspace": "box"},
            "pattern": ".example.com", "pattern_kind": "suffix", "effect": "deny",
            "expires_at": null, "created_at": 5, "created_by": "ui",
            "source_pending_id": 9
        })
    );
    assert_eq!(
        serde_json::to_value(RuleScope::Global).unwrap(),
        json!({"type": "global"})
    );
    // Input is normalised, not refused: case, punycode, a trailing dot, a wildcard.
    let typed = NewRuleRequest {
        scope: RuleScope::Global,
        pattern: "  *.Bücher.Example.COM. ".into(),
        effect: Effect::Allow,
        expires_at: None,
    };
    assert_eq!(
        typed.into_new_rule().unwrap().pattern.to_string(),
        ".xn--bcher-kva.example.com"
    );
    let new = NewRuleRequest {
        scope: RuleScope::Global,
        pattern: "example.com".into(),
        effect: Effect::Allow,
        expires_at: None,
    }
    .into_new_rule()
    .unwrap();
    assert_eq!(new.pattern.to_string(), "example.com");
    assert_eq!(new.created_by, store::Actor::Api);
    assert!(
        NewRuleRequest {
            scope: RuleScope::Global,
            pattern: "*.com".into(),
            effect: Effect::Allow,
            expires_at: None,
        }
        .into_new_rule()
        .is_err()
    );
}

#[test]
fn pending_rows_always_carry_every_field() {
    let row = store::PendingRow {
        id: PendingId(1),
        workspace: WorkspaceId::new("box").unwrap(),
        host: Host::parse_normalised("example.com").unwrap(),
        port: 443,
        first_seen: 1,
        last_seen: 2,
        attempts: 3,
        state: store::PendingState::Requested,
        decided_at: None,
        decided_by: None,
        rule_id: None,
        rule_set: Some("system".into()),
    };
    let v = serde_json::to_value(PendingRequest::from_store(row).unwrap()).unwrap();
    assert_eq!(v["rule_set"], "system");
    assert_eq!(v["decided_at"], Value::Null);
    assert_eq!(v["decided_by"], Value::Null);
    assert_eq!(v["rule_id"], Value::Null);
    assert_eq!(v["state"], "requested");
    for (state, text) in [
        (store::PendingState::Allowed, "allowed"),
        (store::PendingState::Denied, "denied"),
        (store::PendingState::Expired, "expired"),
    ] {
        assert_eq!(
            serde_json::to_value(PendingState::from(state)).unwrap(),
            text
        );
    }
    for (actor, text) in [
        (store::Actor::Cli, "cli"),
        (store::Actor::Api, "api"),
        (store::Actor::System, "system"),
    ] {
        assert_eq!(
            serde_json::to_value(Actor::from_store(actor).unwrap()).unwrap(),
            text
        );
    }
}

#[test]
fn consents_map_and_validate_terms() {
    let granted = ConsentRequest {
        decision: ConsentDecision::Granted,
        terms_version: "v1".into(),
    }
    .into_consent(42)
    .unwrap();
    assert_eq!(
        serde_json::to_value(Consent::from(&granted)).unwrap(),
        json!({"state": "granted", "at": 42, "terms_version": "v1"})
    );
    let declined = ConsentRequest {
        decision: ConsentDecision::Declined,
        terms_version: "v1".into(),
    }
    .into_consent(1)
    .unwrap();
    assert!(matches!(Consent::from(&declined), Consent::Declined { .. }));
    assert!(
        ConsentRequest {
            decision: ConsentDecision::Granted,
            terms_version: "has space".into(),
        }
        .into_consent(1)
        .is_err()
    );
    let mut all = settings::Consents::default();
    all.set(
        settings::ConsentKind::from(ConsentKind::CrashReports),
        granted,
    );
    let wire = Consents::from(&all);
    assert_eq!(wire.telemetry, Consent::NotAsked);
    assert!(matches!(wire.crash_reports, Consent::Granted { .. }));
    assert_eq!(
        settings::ConsentKind::from(ConsentKind::VscodeServer),
        settings::ConsentKind::VsCodeServer
    );
    assert_eq!(
        serde_json::to_value(ConsentKind::VscodeServer).unwrap(),
        "vscode_server"
    );
}

#[test]
fn clipboard_and_effect_map_both_ways() {
    for c in [
        ClipboardRead::Ask,
        ClipboardRead::Allow,
        ClipboardRead::Deny,
    ] {
        assert_eq!(ClipboardRead::from(settings::ClipboardRead::from(c)), c);
    }
    for e in [Effect::Allow, Effect::Deny] {
        assert_eq!(Effect::from(store::Effect::from(e)), e);
    }
}

#[test]
fn patterns_are_normalised_with_the_proxy_normaliser() {
    for (input, want) in [
        ("example.com", (false, "example.com")),
        ("EXAMPLE.com.", (false, "example.com")),
        (".example.com", (true, "example.com")),
        ("*.Example.Com", (true, "example.com")),
        ("Bücher.example", (false, "xn--bcher-kva.example")),
        ("\t10.0.0.1\n", (false, "10.0.0.1")),
        ("[2001:DB8::1]", (false, "2001:db8::1")),
        ("2001:DB8:0::1", (false, "2001:db8::1")),
    ] {
        assert_eq!(
            normalise_pattern("pattern", input).unwrap(),
            (want.0, want.1.to_owned()),
            "{input:?}"
        );
    }
}

#[test]
fn patterns_that_are_not_hosts_are_refused_naming_the_field() {
    for (input, why) in [
        ("", "invalid host"),
        ("https://example.com/a", "not a URL"),
        ("user@example.com", "not a URL"),
        ("example.com/path", "not a URL"),
        ("example.com:443", "leave the port out"),
        ("*.example.com:8080", "leave the port out"),
        ("a b.example", "invalid host"),
        ("127.1", "canonical"),
        ("exa*mple.com", "invalid host"),
        ("-.", "invalid host"),
    ] {
        let err = normalise_pattern("suffix", input).unwrap_err();
        assert_eq!(err.status(), axum::http::StatusCode::UNPROCESSABLE_ENTITY);
        let body = format!("{err:?}");
        assert!(body.contains("suffix: "), "{input:?}: {body}");
        assert!(body.contains(why), "{input:?}: {body}");
    }
}

#[test]
fn a_decision_suffix_is_normalised_too() {
    let r = DecisionRequest {
        scope: ScopeChoice::Workspace,
        rule_set: None,
        suffix: Some("*.GitHub.COM".into()),
        expires_in_secs: None,
    }
    .into_resolution(Effect::Allow)
    .unwrap();
    assert_eq!(r.pattern, store::PatternChoice::Suffix("github.com".into()));
    assert!(
        DecisionRequest {
            scope: ScopeChoice::Workspace,
            rule_set: None,
            suffix: Some("https://github.com".into()),
            expires_in_secs: None,
        }
        .into_resolution(Effect::Allow)
        .is_err()
    );
}

#[test]
fn audit_types_and_outcomes_cover_the_stores() {
    let tags: Vec<&str> = [
        AuditType::Connection,
        AuditType::PendingCreated,
        AuditType::PendingDecided,
        AuditType::PendingExpired,
        AuditType::PendingSuppressed,
        AuditType::RuleCreated,
        AuditType::RuleUpdated,
        AuditType::RuleDeleted,
        AuditType::RuleExpired,
        AuditType::RequestRuleCreated,
        AuditType::RequestRuleDeleted,
        AuditType::RuleSetCreated,
        AuditType::RuleSetUpdated,
        AuditType::RuleSetDeleted,
        AuditType::RuleSetSwitched,
        AuditType::RuleSetChanged,
        AuditType::SystemManagedChanged,
        AuditType::WorkspaceOp,
        AuditType::AuditTrimmed,
    ]
    .into_iter()
    .map(AuditType::tag)
    .collect();
    assert_eq!(tags, store::AuditRecord::KINDS);
    for kind in [AuditType::PendingCreated, AuditType::RuleExpired] {
        assert_eq!(serde_json::to_value(kind).unwrap(), kind.tag());
    }
    for (wire, domain) in [
        (AuditOutcome::Allow, store::AuditOutcome::Allow),
        (AuditOutcome::Deny, store::AuditOutcome::Deny),
        (AuditOutcome::Pending, store::AuditOutcome::Pending),
        (AuditOutcome::Blocked, store::AuditOutcome::Blocked),
        (AuditOutcome::Expired, store::AuditOutcome::Expired),
        (AuditOutcome::Succeeded, store::AuditOutcome::Succeeded),
        (AuditOutcome::Failed, store::AuditOutcome::Failed),
    ] {
        assert_eq!(
            serde_json::to_value(wire).unwrap(),
            domain.as_str(),
            "{wire:?}"
        );
        assert_eq!(store::AuditOutcome::from(wire), domain);
    }
}

#[test]
fn rule_set_records_serialise_as_stored() {
    let set = serde_json::json!({
        "id": 3, "name": "Work", "description": "", "created_at": 1, "created_by": "ui",
        "fill": {"kind": "solid", "color": "#0a6a85"}
    });
    let rule = serde_json::json!({
        "id": 9, "scope": "set", "workspace_id": null, "set_id": 3, "pattern_kind": "exact",
        "pattern": "a.example", "effect": "deny", "expires_at": null, "created_at": 1,
        "created_by": "ui", "source_pending_id": null
    });
    let pending = serde_json::json!({
        "id": 2, "workspace_id": "box", "host": "open-vsx.org", "port": 443, "first_seen": 1,
        "last_seen": 1, "attempts": 1, "state": "allowed", "decided_at": 2,
        "decided_by": "system", "rule_id": null, "rule_set": "system"
    });
    for stored in [
        serde_json::json!({"type": "rule_set_created", "ts": 1, "rule_set": set, "actor": "ui"}),
        serde_json::json!({"type": "rule_set_updated", "ts": 1, "before": set, "rule_set": set, "actor": "api"}),
        serde_json::json!({"type": "rule_set_deleted", "ts": 1, "rule_set": set, "actor": "ui"}),
        serde_json::json!({"type": "rule_set_switched", "ts": 1, "set_id": "builtin:github",
                "workspace_id": null, "enabled": null, "actor": "ui"}),
        serde_json::json!({"type": "rule_set_changed", "ts": 1, "set_id": "builtin:github",
                "added": ["api.github.com"], "removed": []}),
        serde_json::json!({"type": "system_managed_changed", "ts": 1, "workspace_id": "box",
                "added": ["direct_ssh"], "removed": ["code_server"]}),
        serde_json::json!({"type": "rule_deleted", "ts": 1, "rule": rule, "reason": "set_deleted",
                "actor": "ui"}),
        serde_json::json!({"type": "pending_decided", "ts": 1, "pending": pending}),
    ] {
        let record: store::AuditRecord = serde_json::from_value(stored.clone()).unwrap();
        assert_eq!(
            serde_json::to_value(AuditRecord::from(record)).unwrap(),
            stored
        );
    }
    // A set recorded before sets had a colour reads, and the wire says `null` for it.
    let old = serde_json::json!({"type": "rule_set_created", "ts": 1, "actor": "ui",
            "rule_set": {"id": 3, "name": "Work", "description": "", "created_at": 1,
                         "created_by": "ui"}});
    let record: store::AuditRecord = serde_json::from_value(old).unwrap();
    assert_eq!(
        serde_json::to_value(AuditRecord::from(record)).unwrap()["rule_set"]["fill"],
        Value::Null
    );
    // A reason a later Puddle added is left out rather than failing the page.
    let newer = serde_json::json!({"type": "system_managed_changed", "ts": 1,
            "workspace_id": null, "added": ["zed"], "removed": []});
    let record: store::AuditRecord = serde_json::from_value(newer).unwrap();
    assert_eq!(
        serde_json::to_value(AuditRecord::from(record)).unwrap()["added"],
        serde_json::json!([])
    );
}

/// The wire carries the refused address and the code of an IP-rule deny as stored.
#[test]
fn an_ip_rule_refusal_reaches_the_wire() {
    let stored = serde_json::json!({
        "type": "connection", "ts": 5, "workspace_id": "box", "origin": "workspace", "host": "example.com",
        "port": 443, "resolved_ip": "10.0.0.9", "decision": "deny", "reason": "rule", "rule_id": 4,
        "refused_address": "10.0.0.9", "refused_reason": "ip_rule", "pending_id": null, "binding_id": null,
        "injected": false, "unswapped_stand_ins": [], "scrubbed_stand_ins": [], "foreign_credentials": [], "answer_credentials": [], "method": null, "path": null,
        "path_truncated": false, "bytes_up": 0, "bytes_down": 0, "count": null
    });
    let record: store::AuditRecord = serde_json::from_value(stored).unwrap();
    let wire = serde_json::to_value(AuditRecord::from(record)).unwrap();
    assert_eq!(wire["refused_address"], "10.0.0.9");
    assert_eq!(wire["refused_reason"], "ip_rule");
}

/// The kinds of foreign credential reach the wire as stored, and a record written before the
/// field existed reads as none.
#[test]
fn foreign_credential_kinds_reach_the_wire_and_an_old_record_has_none() {
    let mut stored = serde_json::json!({
        "type": "connection", "ts": 5, "workspace_id": "box", "origin": "workspace", "host": "example.com",
        "port": 443, "resolved_ip": null, "decision": "allow", "reason": "rule", "rule_id": 4,
        "pending_id": null, "binding_id": null, "injected": false, "method": null, "path": null,
        "path_truncated": false, "bytes_up": 0, "bytes_down": 0, "count": null
    });
    let old: store::AuditRecord = serde_json::from_value(stored.clone()).unwrap();
    let wire = serde_json::to_value(AuditRecord::from(old)).unwrap();
    assert_eq!(wire["foreign_credentials"], serde_json::json!([]));
    stored["foreign_credentials"] = serde_json::json!(["github_token", "git_authorization"]);
    let record: store::AuditRecord = serde_json::from_value(stored).unwrap();
    let wire = serde_json::to_value(AuditRecord::from(record)).unwrap();
    assert_eq!(
        wire["foreign_credentials"],
        serde_json::json!(["github_token", "git_authorization"])
    );
}

/// The kinds of unknown token reach the wire as stored, and an old record has none.
#[test]
fn unknown_token_kinds_reach_the_wire_and_an_old_record_has_none() {
    let mut stored = serde_json::json!({
        "type": "connection", "ts": 5, "workspace_id": "box", "origin": "workspace", "host": "example.com",
        "port": 443, "resolved_ip": null, "decision": "allow", "reason": "rule", "rule_id": 4,
        "pending_id": null, "binding_id": null, "injected": false, "method": null, "path": null,
        "path_truncated": false, "bytes_up": 0, "bytes_down": 0, "count": null
    });
    let old: store::AuditRecord = serde_json::from_value(stored.clone()).unwrap();
    let wire = serde_json::to_value(AuditRecord::from(old)).unwrap();
    assert_eq!(wire["answer_credentials"], serde_json::json!([]));
    stored["answer_credentials"] = serde_json::json!(["npm_token"]);
    let record: store::AuditRecord = serde_json::from_value(stored).unwrap();
    let wire = serde_json::to_value(AuditRecord::from(record)).unwrap();
    assert_eq!(wire["answer_credentials"], serde_json::json!(["npm_token"]));
}

/// A record written before the company-network fields existed reads them as `null`.
#[test]
fn a_record_without_a_category_reads_as_none() {
    let stored = serde_json::json!({
        "type": "connection", "ts": 5, "workspace_id": "box", "origin": "workspace", "host": "example.com",
        "port": 443, "decision": "deny", "reason": "toggle:company_network", "rule_id": null,
        "pending_id": null, "binding_id": null, "injected": false, "unswapped_stand_ins": [],
        "method": null, "path": null, "path_truncated": false, "bytes_up": 0, "bytes_down": 0, "count": null
    });
    let record: store::AuditRecord = serde_json::from_value(stored).unwrap();
    let wire = serde_json::to_value(AuditRecord::from(record)).unwrap();
    assert_eq!(wire["category"], Value::Null);
    assert_eq!(wire["signal"], Value::Null);
    assert_eq!(wire["reason"], "toggle:company_network");
}

/// The typed record serialises to the very JSON the store wrote, so the API adds no layer
/// that could drop or rename a field, including `upstream` and records older than it.
#[test]
fn typed_audit_records_serialise_as_stored() {
    let stored = serde_json::json!({
        "type": "connection", "ts": 5, "workspace_id": "box", "origin": "workspace", "host": "example.com",
        "port": 443, "resolved_ip": "93.184.216.34", "upstream": "PROXY corp:3128",
        "decision": "allow", "outcome": "connected", "reason": "rule", "rule_id": 4, "rule_set": "user:2",
        "request_rule": 3, "refused_address": "10.0.0.8", "refused_reason": "toggle:private", "category": "company_network", "signal": "dns_suffix", "pending_id": null, "binding_id": null, "injected": false, "unswapped_stand_ins": [], "scrubbed_stand_ins": [], "foreign_credentials": [], "answer_credentials": [],
        "method": "GET", "path": "/",
        "path_truncated": false, "bytes_up": 1, "bytes_down": 2, "count": null
    });
    let record: store::AuditRecord = serde_json::from_value(stored.clone()).unwrap();
    assert_eq!(
        serde_json::to_value(AuditRecord::from(record)).unwrap(),
        stored
    );
    // Written before `request_rule` existed.
    let mut before_rules = stored.clone();
    before_rules.as_object_mut().unwrap().remove("request_rule");
    let record: store::AuditRecord = serde_json::from_value(before_rules).unwrap();
    let back = serde_json::to_value(AuditRecord::from(record)).unwrap();
    assert_eq!(back["request_rule"], Value::Null);
    // Written before `upstream` existed.
    let mut old = stored;
    old.as_object_mut().unwrap().remove("upstream");
    let record: store::AuditRecord = serde_json::from_value(old.clone()).unwrap();
    let mut back = serde_json::to_value(AuditRecord::from(record)).unwrap();
    assert_eq!(back["upstream"], Value::Null);
    back.as_object_mut().unwrap().remove("upstream");
    assert_eq!(back, old);
    // Written before `origin` existed: a workspace's connection.
    let mut older = serde_json::to_value(AuditRecord::from(
        serde_json::from_value::<store::AuditRecord>(old).unwrap(),
    ))
    .unwrap();
    older.as_object_mut().unwrap().remove("origin");
    let record: store::AuditRecord = serde_json::from_value(older).unwrap();
    let back = serde_json::to_value(AuditRecord::from(record)).unwrap();
    assert_eq!(back["origin"], "workspace");
    assert_eq!(back["workspace_id"], "box");
    // Puddle's own connection: no workspace.
    let mut own = back;
    own["origin"] = "puddle".into();
    own["workspace_id"] = Value::Null;
    let record: store::AuditRecord = serde_json::from_value(own.clone()).unwrap();
    assert_eq!(
        serde_json::to_value(AuditRecord::from(record)).unwrap(),
        own
    );
}
