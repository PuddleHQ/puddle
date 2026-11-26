// SPDX-License-Identifier: AGPL-3.0-or-later
//! The editor defaults on a real microVM (K on Linux KVM): the host plans the file, the agent
//! merges it into code-server's own settings before the server starts, and a later start never
//! overwrites a key the user changed in the guest.
#![cfg(target_os = "linux")]

use puddle_host_test_support::support;
use puddle_host_test_support::vm_support;

use std::time::Duration;

use puddle_types::WorkspaceId;
use serde_json::{Value, json};
use support::{Api, ended, named_id_options};
use vm_support::{VmHost, guest, start_vm_host_config};

/// Waits until the editor server answers its health check.
const EDITOR_UP: &str = "for i in $(seq 1 100); do curl -sf -o /dev/null http://127.0.0.1:41337/healthz && exit 0; sleep 1; done; exit 1";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(clippy::too_many_lines, reason = "one story, read top to bottom")]
async fn vm_editor_defaults_are_merged_before_the_server_starts_and_a_users_own_key_is_never_overwritten()
 {
    let release = std::path::PathBuf::from(std::env::var_os("PUDDLE_CODE_SERVER_DIR").expect(
        "PUDDLE_CODE_SERVER_DIR: the unpacked code-server release (ci/fetch-code-server.sh)",
    ));
    let VmHost {
        host,
        runtime,
        settings,
        ..
    } = start_vm_host_config("edef", named_id_options(), |config| {
        config.guest.code_server = Some(release);
    })
    .await;
    let name = format!("{}-edef", settings.prefix.as_str());
    let sandbox = WorkspaceId::new(&name).unwrap();
    let api = Api::new(host.url(), host.token());
    let user_dir = "/workspace/.puddle/code-server/user-data/User".to_owned();

    let put = |editor: Value| {
        let api = &api;
        async move {
            let reply = api
                .put(
                    "/api/settings",
                    &json!({"workspace_defaults": {"editor": editor}}).to_string(),
                )
                .await;
            assert_eq!(reply.status, 200, "{}", reply.body);
        }
    };
    put(json!({
        "colour_theme": "Default Dark Modern",
        "font_size": 17,
        "keybindings": r#"[{"key":"ctrl+alt+9","command":"workbench.action.toggleSidebarVisibility"}]"#
    }))
    .await;

    let mut events = api.events().await;
    let name_static: &'static str = Box::leak(name.clone().into_boxed_str());
    let reply = api
        .post(
            "/api/workspaces",
            &json!({"name": name, "repositories": [], "memory_mib": 2048}).to_string(),
        )
        .await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    let end = events
        .until(ended(name_static), Duration::from_secs(600))
        .await;
    assert_eq!(end["step"], "done", "{end}");
    guest(&runtime, &sandbox, EDITOR_UP).await;

    // The server started with the merged files: read them as the guest holds them.
    let settings_text = guest(&runtime, &sandbox, &format!("cat {user_dir}/settings.json")).await;
    let merged: Value = serde_json::from_str(&settings_text)
        .unwrap_or_else(|e| panic!("settings.json is not strict JSON here ({e}): {settings_text}"));
    assert_eq!(merged["workbench.colorTheme"], "Default Dark Modern");
    assert_eq!(merged["editor.fontSize"], 17);
    assert_eq!(merged["window.autoDetectColorScheme"], false);
    let keys = guest(
        &runtime,
        &sandbox,
        &format!("cat {user_dir}/keybindings.json"),
    )
    .await;
    assert!(keys.contains("ctrl+alt+9"), "{keys}");
    // The files belong to the editor's user, who can save them again.
    let owner = guest(
        &runtime,
        &sandbox,
        &format!("stat -c %U {user_dir}/settings.json"),
    )
    .await;
    assert_eq!(owner.trim(), "agent");

    // The user picks their own size in the editor; the default theme later changes.
    guest(
        &runtime,
        &sandbox,
        &format!("sed -i 's/\"editor.fontSize\": 17/\"editor.fontSize\": 22/' {user_dir}/settings.json && grep -q '\"editor.fontSize\": 22' {user_dir}/settings.json"),
    )
    .await;
    put(json!({
        "colour_theme": "Default Light Modern",
        "font_size": 12,
        "keybindings": r#"[{"key":"ctrl+alt+9","command":"workbench.action.toggleSidebarVisibility"}]"#
    }))
    .await;
    for step in ["stop", "start"] {
        let reply = api
            .post(&format!("/api/workspaces/{name}/{step}"), "")
            .await;
        assert_eq!(reply.status, 202, "{}", reply.body);
        let end = events
            .until(ended(name_static), Duration::from_secs(300))
            .await;
        assert_eq!(end["step"], "done", "{end}");
    }
    guest(&runtime, &sandbox, EDITOR_UP).await;
    let text = guest(&runtime, &sandbox, &format!("cat {user_dir}/settings.json")).await;
    let after: Value = serde_json::from_str(&text).unwrap_or_else(|e| panic!("{e}: {text}"));
    assert_eq!(
        after["editor.fontSize"], 22,
        "the user's size stays: {text}"
    );
    assert_eq!(
        after["workbench.colorTheme"], "Default Light Modern",
        "an untouched key follows the new default: {text}"
    );
    let keys = guest(
        &runtime,
        &sandbox,
        &format!("cat {user_dir}/keybindings.json"),
    )
    .await;
    assert_eq!(
        keys.matches("ctrl+alt+9").count(),
        1,
        "no duplicate: {keys}"
    );

    let down = host.shutdown().await;
    assert!(down.sandboxes.all_stopped(), "{down:?}");
}
