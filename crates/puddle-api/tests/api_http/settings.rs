// SPDX-License-Identifier: GPL-3.0-or-later
//! Settings and consents through the API, over the versioned documents of `puddle-settings`.

use puddle_api::SettingsRepo;
use puddle_types::WorkspaceName;
use serde_json::{Value, json};

use crate::common::{START_MS, start};

/// The terms the enable popup shows (`puddle_settings::MICROSOFT_TERMS_VERSION`).
const MS_TERMS: &str = puddle_settings::MICROSOFT_TERMS_VERSION;

fn null_layer() -> Value {
    json!({
        "memory": null,
        "cpus": null,
        "local_toggles": {"loopback": null, "private": null, "link_local": null, "metadata": null, "special": null},
        "wildcards_reach_local": null,
        "reconnection_grace": null,
        "zoom_hotkeys": null,
        "clipboard_read": null,
        "direct_ssh": null,
        "capture_logins": null
    })
}

#[tokio::test]
async fn fresh_settings_are_all_defaults_with_every_field_present() {
    let api = start().await;
    let reply = api.get("/api/settings").await;
    assert_eq!(reply.status, 200);
    let view = reply.json();
    assert_eq!(view["workspace_defaults"], null_layer());
    assert_eq!(
        view["vscode_server"],
        json!({"server": null, "telemetry": null, "auto_update": null})
    );
    assert_eq!(
        view["ui"],
        json!({"theme": null, "density": null, "palette": null, "custom_accent": null, "custom_background": null,
               "custom_surface": null, "custom_text": null, "notifications": null, "sound": null, "close_behaviour": null,
               "minimize_to_tray": null, "app_icon": null, "notify_only_unfocused": null, "badge": null, "notify_oom": null, "notify_unexpected_stop": null,
               "confirm_global_rules": null, "inbox_shortcuts": null, "start_at_login": null})
    );
    assert_eq!(view["unknown_fields"], json!([]));
    let e = &view["effective"];
    assert_eq!(e["memory"], json!({"value": 8192, "source": "default"}));
    assert_eq!(e["cpus"], json!({"value": 1, "source": "default"}));
    assert_eq!(e["reconnection_grace"]["value"], 300);
    assert_eq!(e["clipboard_read"]["value"], "ask");
    assert_eq!(e["local_toggles"]["metadata"]["value"], false);
    assert_eq!(
        e["direct_ssh"],
        json!({"value": false, "source": "default"})
    );
    assert_eq!(
        e["capture_logins"],
        json!({"value": true, "source": "default"}),
        "login capture is on until someone turns it off"
    );
    // Reading wrote nothing.
    assert_eq!(api.settings.load_global().unwrap(), None);
    api.running.shutdown().await;
}

#[tokio::test]
async fn global_and_workspace_values_resolve_in_order() {
    let api = start().await;
    let reply = api
        .send(
            "PUT",
            "/api/settings",
            Some(&json!({
                "workspace_defaults": {"memory": 4096, "cpus": 2, "local_toggles": {"private": true}},
                "vscode_server": {"telemetry": true}
            })),
        )
        .await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    let view = reply.json();
    assert_eq!(view["workspace_defaults"]["memory"], 4096);
    assert_eq!(view["effective"]["memory"]["source"], "global");
    assert_eq!(
        view["effective"]["cpus"],
        json!({"value": 2, "source": "global"})
    );
    assert_eq!(view["vscode_server"]["telemetry"], true);

    let reply = api
        .send(
            "PUT",
            "/api/settings/workspaces/big",
            Some(&json!({"overrides": {"memory": 16384, "cpus": 4, "clipboard_read": "deny", "capture_logins": false}})),
        )
        .await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    let view = reply.json();
    assert_eq!(view["workspace"], "big");
    assert_eq!(
        view["effective"]["memory"],
        json!({"value": 16384, "source": "workspace"})
    );
    assert_eq!(
        view["effective"]["cpus"],
        json!({"value": 4, "source": "workspace"})
    );
    assert_eq!(view["effective"]["clipboard_read"]["source"], "workspace");
    assert_eq!(
        view["effective"]["capture_logins"],
        json!({"value": false, "source": "workspace"})
    );
    assert_eq!(
        view["effective"]["local_toggles"]["private"],
        json!({"value": true, "source": "global"})
    );
    assert_eq!(view["effective"]["zoom_hotkeys"]["source"], "default");

    // A workspace without stored settings inherits; reading it doesn't create a document.
    let plain = api.get("/api/settings/workspaces/plain").await.json();
    assert_eq!(plain["overrides"], null_layer());
    assert_eq!(plain["effective"]["memory"]["value"], 4096);
    assert_eq!(plain["effective"]["cpus"]["value"], 2);
    assert_eq!(plain["effective"]["capture_logins"]["value"], true);
    let name = WorkspaceName::new("plain").unwrap();
    assert_eq!(api.settings.load_workspace(&name).unwrap(), None);

    // Stored documents are sparse and versioned.
    assert_eq!(
        api.settings.load_global().unwrap().unwrap(),
        json!({
            "schema_version": 2,
            "workspace_defaults": {"memory": 4096, "cpus": 2, "local_toggles": {"private": true}},
            "vscode_server": {"telemetry": true}
        })
    );

    // PUT replaces: leaving a value out clears it back to inherit.
    let reply = api.send("PUT", "/api/settings", Some(&json!({}))).await;
    assert_eq!(reply.json()["workspace_defaults"], null_layer());
    api.running.shutdown().await;
}

#[tokio::test]
async fn out_of_range_and_unknown_settings_are_refused() {
    let api = start().await;
    for body in [
        json!({"workspace_defaults": {"memory": 1}}),
        json!({"workspace_defaults": {"cpus": 0}}),
        json!({"workspace_defaults": {"cpus": 33}}),
        json!({"workspace_defaults": {"reconnection_grace": 5}}),
        json!({"workspace_defaults": {"clipboard_read": "sometimes"}}),
        json!({"workspace_defaults": {"swap": 4}}),
        json!({"consents": {}}),
    ] {
        let reply = api.send("PUT", "/api/settings", Some(&body)).await;
        assert_eq!(reply.status, 422, "{body}: {}", reply.body);
    }
    assert_eq!(api.settings.load_global().unwrap(), None);
    let reply = api
        .send("PUT", "/api/settings/workspaces/tauri", Some(&json!({})))
        .await;
    assert_eq!(reply.status, 400, "a reserved workspace name");
    api.running.shutdown().await;
}

#[tokio::test]
async fn unknown_stored_fields_are_listed_and_kept() {
    let api = start().await;
    api.settings
        .save_global(
            json!({"schema_version": 1, "rule_sets": ["x"], "workspace_defaults": {"swap": 2}}),
        )
        .unwrap();
    let name = WorkspaceName::new("box").unwrap();
    api.settings
        .save_workspace(&name, json!({"overrides": {"gpu": true}}))
        .unwrap();
    let view = api.get("/api/settings").await.json();
    assert_eq!(
        view["unknown_fields"],
        json!(["rule_sets", "workspace_defaults.swap"])
    );
    let reply = api
        .send(
            "PUT",
            "/api/settings",
            Some(&json!({"workspace_defaults": {"zoom_hotkeys": false}})),
        )
        .await;
    assert_eq!(reply.status, 200);
    let stored = api.settings.load_global().unwrap().unwrap();
    assert_eq!(stored["rule_sets"], json!(["x"]));
    assert_eq!(stored["workspace_defaults"]["swap"], 2);
    assert_eq!(stored["workspace_defaults"]["zoom_hotkeys"], false);

    let view = api.get("/api/settings/workspaces/box").await.json();
    assert_eq!(view["unknown_fields"], json!(["overrides.gpu"]));
    api.send(
        "PUT",
        "/api/settings/workspaces/box",
        Some(&json!({"overrides": {"memory": 2048}})),
    )
    .await;
    let stored = api.settings.load_workspace(&name).unwrap().unwrap();
    assert_eq!(stored["overrides"], json!({"gpu": true, "memory": 2048}));
    api.running.shutdown().await;
}

#[tokio::test]
async fn settings_from_a_newer_puddle_are_a_conflict_and_left_alone() {
    let api = start().await;
    let newer = json!({"schema_version": 99, "workspace_defaults": {"memory": 4096}});
    api.settings.save_global(newer.clone()).unwrap();
    for (method, path, body) in [
        ("GET", "/api/settings", None),
        ("PUT", "/api/settings", Some(json!({}))),
        ("GET", "/api/consents", None),
        (
            "PUT",
            "/api/consents/telemetry",
            Some(json!({"decision": "granted", "terms_version": "v1"})),
        ),
        ("GET", "/api/settings/workspaces/box", None),
    ] {
        let reply = api.send(method, path, body.as_ref()).await;
        assert_eq!(reply.status, 409, "{method} {path}: {}", reply.body);
        assert_eq!(reply.error(), "newer_settings");
        assert!(
            reply.json()["message"]
                .as_str()
                .unwrap()
                .contains("update Puddle")
        );
    }
    assert_eq!(api.settings.load_global().unwrap(), Some(newer));
    api.running.shutdown().await;
}

#[tokio::test]
async fn damaged_settings_are_a_conflict_that_says_what_is_wrong() {
    let api = start().await;
    // A stored document that isn't an object: the body carries the reason, not a bare 500.
    api.settings.save_global(json!([1, 2])).unwrap();
    for (method, path, body) in [
        ("GET", "/api/settings", None),
        ("PUT", "/api/settings", Some(json!({}))),
        ("GET", "/api/consents", None),
        ("GET", "/api/settings/workspaces/box", None),
    ] {
        let reply = api.send(method, path, body.as_ref()).await;
        assert_eq!(reply.status, 409, "{method} {path}: {}", reply.body);
        assert_eq!(reply.error(), "settings_unreadable");
        let message = reply.json()["message"].as_str().unwrap().to_owned();
        assert!(message.contains("the global settings"), "{message}");
        assert!(message.contains("must be an object"), "{message}");
    }
    assert_eq!(api.settings.load_global().unwrap(), Some(json!([1, 2])));

    // One workspace's damaged document names that workspace, and only its page fails.
    api.settings.save_global(json!({})).unwrap();
    api.settings
        .save_workspace(&WorkspaceName::new("box").unwrap(), json!("x"))
        .unwrap();
    let reply = api.get("/api/settings/workspaces/box").await;
    assert_eq!(reply.status, 409, "{}", reply.body);
    assert_eq!(reply.error(), "settings_unreadable");
    let message = reply.json()["message"].as_str().unwrap().to_owned();
    assert!(message.contains("workspace box"), "{message}");
    assert_eq!(api.get("/api/settings").await.status, 200);
    api.running.shutdown().await;
}

#[tokio::test]
async fn consents_are_recorded_with_the_server_time() {
    let api = start().await;
    let all = api.get("/api/consents").await.json();
    assert_eq!(
        all,
        json!({
            "telemetry": {"state": "not_asked"},
            "crash_reports": {"state": "not_asked"},
            "vscode_server": {"state": "not_asked"}
        })
    );
    api.clock.set(START_MS + 5);
    let reply = api
        .send(
            "PUT",
            "/api/consents/vscode_server",
            Some(&json!({"decision": "granted", "terms_version": "https://example.test/license"})),
        )
        .await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    assert_eq!(
        reply.json()["vscode_server"],
        json!({"state": "granted", "at": START_MS + 5, "terms_version": "https://example.test/license"})
    );
    let reply = api
        .send(
            "PUT",
            "/api/consents/telemetry",
            Some(&json!({"decision": "declined", "terms_version": "2026-10"})),
        )
        .await;
    assert_eq!(reply.json()["telemetry"]["state"], "declined");
    // Recorded in the global document, next to the settings.
    let stored = api.settings.load_global().unwrap().unwrap();
    assert_eq!(stored["consents"]["vscode_server"]["state"], "granted");

    for (path, body, status) in [
        (
            "/api/consents/telemetry",
            json!({"decision": "granted", "terms_version": "has space"}),
            422,
        ),
        (
            "/api/consents/telemetry",
            json!({"decision": "granted", "terms_version": ""}),
            422,
        ),
        (
            "/api/consents/telemetry",
            json!({"decision": "maybe", "terms_version": "v1"}),
            422,
        ),
        (
            "/api/consents/newsletter",
            json!({"decision": "granted", "terms_version": "v1"}),
            400,
        ),
    ] {
        let reply = api.send("PUT", path, Some(&body)).await;
        assert_eq!(reply.status, status, "{path} {body}: {}", reply.body);
    }
    api.running.shutdown().await;
}

#[tokio::test]
async fn microsoft_server_needs_consent_and_ui_prefs_are_stored() {
    let api = start().await;
    let ms = json!({"vscode_server": {"server": "microsoft"}});
    let reply = api.send("PUT", "/api/settings", Some(&ms)).await;
    assert_eq!(reply.status, 422, "{}", reply.body);
    assert!(reply.body.contains("consent"), "{}", reply.body);
    assert_eq!(
        api.get("/api/settings").await.json()["vscode_server"]["server"],
        Value::Null
    );

    // A decline is not consent either.
    let declined = json!({"decision": "declined", "terms_version": MS_TERMS});
    api.send("PUT", "/api/consents/vscode_server", Some(&declined))
        .await;
    assert_eq!(
        api.send("PUT", "/api/settings", Some(&ms)).await.status,
        422
    );

    // Nor is an agreement to other terms than the ones the popup shows today.
    let other = json!({"decision": "granted", "terms_version": "https://example.test/terms"});
    api.send("PUT", "/api/consents/vscode_server", Some(&other))
        .await;
    let reply = api.send("PUT", "/api/settings", Some(&ms)).await;
    assert_eq!(reply.status, 422, "{}", reply.body);

    let granted = json!({"decision": "granted", "terms_version": MS_TERMS});
    api.send("PUT", "/api/consents/vscode_server", Some(&granted))
        .await;
    let body = json!({
        "vscode_server": {"server": "microsoft", "telemetry": false},
        "ui": {"theme": "dark", "density": "compact", "palette": "custom", "custom_accent": "#0A6A85", "custom_text": "#102030", "notifications": false, "sound": true, "close_behaviour": "quit",
                "minimize_to_tray": true, "app_icon": "duck_night", "notify_only_unfocused": false, "badge": false, "notify_oom": false, "notify_unexpected_stop": false,
                "confirm_global_rules": false, "inbox_shortcuts": true, "start_at_login": true}
    });
    let reply = api.send("PUT", "/api/settings", Some(&body)).await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    let view = reply.json();
    assert_eq!(view["vscode_server"]["server"], "microsoft");
    assert_eq!(view["ui"]["theme"], "dark");
    assert_eq!(view["ui"]["close_behaviour"], "quit");
    assert_eq!(view["ui"]["density"], "compact");
    assert_eq!(view["ui"]["palette"], "custom");
    assert_eq!(view["ui"]["custom_accent"], "#0a6a85");
    assert_eq!(view["ui"]["custom_background"], json!(null));
    for key in [
        "notify_only_unfocused",
        "badge",
        "notify_oom",
        "notify_unexpected_stop",
        "confirm_global_rules",
    ] {
        assert_eq!(view["ui"][key], false, "{key}");
    }
    assert_eq!(view["ui"]["inbox_shortcuts"], true);
    assert_eq!(view["ui"]["start_at_login"], true);
    assert_eq!(view["ui"]["app_icon"], "duck_night");
    let stored = api.settings.load_global().unwrap().unwrap();
    assert_eq!(stored["ui"]["theme"], "dark");
    assert_eq!(stored["vscode_server"]["server"], "microsoft");
    // The consent survives choosing code-server again.
    let back = json!({"vscode_server": {"server": "code_server"}});
    assert_eq!(
        api.send("PUT", "/api/settings", Some(&back)).await.status,
        200
    );
    assert_eq!(
        api.get("/api/consents").await.json()["vscode_server"]["state"],
        "granted"
    );

    let bad = json!({"ui": {"theme": "purple"}});
    assert_eq!(
        api.send("PUT", "/api/settings", Some(&bad)).await.status,
        422
    );
    let bad = json!({"ui": {"density": "tight"}});
    assert_eq!(
        api.send("PUT", "/api/settings", Some(&bad)).await.status,
        422
    );
    for bad in [
        json!({"ui": {"palette": "neon"}}),
        json!({"ui": {"custom_accent": "teal"}}),
        json!({"ui": {"custom_surface": "#12345"}}),
    ] {
        assert_eq!(
            api.send("PUT", "/api/settings", Some(&bad)).await.status,
            422,
            "{bad}"
        );
    }
    // A client from before the density field leaves it out: the document stays valid.
    let old = json!({"ui": {"theme": "light", "notifications": null, "sound": null, "close_behaviour": null}});
    let reply = api.send("PUT", "/api/settings", Some(&old)).await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    assert_eq!(reply.json()["ui"]["density"], json!(null));
    assert_eq!(reply.json()["ui"]["palette"], json!(null));
    api.running.shutdown().await;
}

/// A settings document a Puddle from before the rename left behind: its defaults show under the new
/// name and the document is written back in the current shape.
/// Withdrawing the consent for Microsoft's server also takes the server choice back to
/// code-server in the same save, so the stored settings never say "Microsoft" without a consent.
#[tokio::test]
async fn withdrawing_the_microsoft_consent_returns_to_code_server_and_closes_its_hosts() {
    let api = start().await;
    let granted = json!({"decision": "granted", "terms_version": MS_TERMS});
    api.send("PUT", "/api/consents/vscode_server", Some(&granted))
        .await;
    let chosen = json!({"vscode_server": {"server": "microsoft", "telemetry": true}});
    assert_eq!(
        api.send("PUT", "/api/settings", Some(&chosen)).await.status,
        200
    );

    // The consent for another purpose changes nothing about the server.
    let telemetry = json!({"decision": "declined", "terms_version": "t1"});
    api.send("PUT", "/api/consents/telemetry", Some(&telemetry))
        .await;
    assert_eq!(
        api.get("/api/settings").await.json()["vscode_server"]["server"],
        "microsoft"
    );

    let withdrawn = json!({"decision": "declined", "terms_version": MS_TERMS});
    let reply = api
        .send("PUT", "/api/consents/vscode_server", Some(&withdrawn))
        .await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    assert_eq!(reply.json()["vscode_server"]["state"], "declined");
    let settings = api.get("/api/settings").await.json();
    assert_eq!(settings["vscode_server"]["server"], "code_server");
    // The popup's telemetry choice is the user's own and stays for a later grant.
    assert_eq!(settings["vscode_server"]["telemetry"], true);
    let stored = api.settings.load_global().unwrap().unwrap();
    assert_eq!(stored["vscode_server"]["server"], "code_server");
    assert_eq!(stored["consents"]["vscode_server"]["state"], "declined");
    // Choosing Microsoft's server again is refused until the user agrees again.
    assert_eq!(
        api.send("PUT", "/api/settings", Some(&chosen)).await.status,
        422
    );
    api.running.shutdown().await;
}

/// A record for other terms than today's does not cover the server, so a Microsoft choice does
/// not survive it; the same terms recorded again keep the choice.
#[tokio::test]
async fn a_grant_of_other_terms_does_not_keep_a_microsoft_choice() {
    let api = start().await;
    let granted = json!({"decision": "granted", "terms_version": MS_TERMS});
    api.send("PUT", "/api/consents/vscode_server", Some(&granted))
        .await;
    let chosen = json!({"vscode_server": {"server": "microsoft"}});
    api.send("PUT", "/api/settings", Some(&chosen)).await;
    // The same terms granted again (the popup after a reinstall) keep the choice.
    api.send("PUT", "/api/consents/vscode_server", Some(&granted))
        .await;
    assert_eq!(
        api.get("/api/settings").await.json()["vscode_server"]["server"],
        "microsoft"
    );
    // A record for other terms does not cover it.
    let other = json!({"decision": "granted", "terms_version": "https://example.test/terms"});
    api.send("PUT", "/api/consents/vscode_server", Some(&other))
        .await;
    assert_eq!(
        api.get("/api/settings").await.json()["vscode_server"]["server"],
        "code_server"
    );
    api.running.shutdown().await;
}

#[tokio::test]
async fn global_settings_stored_under_the_old_name_are_read_and_rewritten() {
    let api = start().await;
    api.settings
        .save_global(json!({
            "schema_version": 1,
            "sandbox_defaults": {"memory": 4096, "local_toggles": {"private": true}},
            "ui": {"theme": "dark"}
        }))
        .unwrap();
    let view = api.get("/api/settings").await.json();
    assert_eq!(view["workspace_defaults"]["memory"], 4096);
    assert_eq!(view["workspace_defaults"]["local_toggles"]["private"], true);
    assert_eq!(view["ui"]["theme"], "dark");
    assert_eq!(view["unknown_fields"], json!([]));
    assert!(view.get("sandbox_defaults").is_none(), "{view}");
    assert_eq!(
        api.settings.load_global().unwrap().unwrap(),
        json!({
            "schema_version": 2,
            "workspace_defaults": {"memory": 4096, "local_toggles": {"private": true}},
            "ui": {"theme": "dark"}
        })
    );
    api.running.shutdown().await;
}
