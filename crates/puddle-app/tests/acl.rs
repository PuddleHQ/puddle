// SPDX-License-Identifier: GPL-3.0-or-later
//! The main window has no Tauri permissions.
//!
//! Two layers: the capability files say it (static), and Tauri's own authority enforces it
//! (an IPC call from the main window's origin is refused). A change that grants `main` anything
//! fails here, on purpose: it needs a decision, not a drive-by.

#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "helpers outside #[test] fns run only in tests; a panic is how a test fails"
)]

use std::path::{Path, PathBuf};

use puddle_app::window::{MAIN_LABEL, open_main};
use serde_json::Value;
use tauri::WebviewWindow;
use tauri::ipc::{CallbackFn, InvokeBody};
use tauri::test::MockRuntime;
use tauri::test::{INVOKE_KEY, get_ipc_response, mock_builder};
use tauri::webview::InvokeRequest;
use url::Url;

#[tauri::command]
fn ping() -> &'static str {
    "pong"
}

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn json(path: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn capability_files() -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(crate_dir().join("capabilities"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    files.sort();
    files
}

fn strings(value: &Value) -> Vec<&str> {
    value
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default()
}

#[test]
fn only_json_capability_files_exist() {
    // Tauri also reads .toml and .json5 capability files; a format this test doesn't parse
    // would be a way around it.
    let files = capability_files();
    assert!(files.len() == 1, "one capability file: {files:?}");
    for file in files {
        assert_eq!(
            file.extension().and_then(|e| e.to_str()),
            Some("json"),
            "{file:?}"
        );
    }
}

#[test]
fn no_capability_grants_the_main_window_a_permission_or_reaches_a_remote_origin() {
    for file in capability_files() {
        let capability = json(&file);
        let name = file.display();
        assert!(
            capability.get("remote").is_none(),
            "{name}: `remote` is never allowed"
        );
        let windows = strings(&capability["windows"]);
        let webviews = strings(&capability["webviews"]);
        for label in windows.iter().chain(webviews.iter()) {
            assert!(!label.contains('*'), "{name}: wildcard label {label:?}");
        }
        let covers_main = windows.contains(&MAIN_LABEL) || webviews.contains(&MAIN_LABEL);
        if covers_main {
            let permissions = capability["permissions"].as_array().unwrap();
            assert!(
                permissions.is_empty(),
                "{name}: the main window gets no permission: {permissions:?}"
            );
        }
        // Every capability, whatever it covers, is limited to named windows.
        assert!(
            !windows.is_empty() || !webviews.is_empty(),
            "{name}: a capability must name its windows"
        );
    }
}

#[test]
fn the_configuration_enables_only_the_capabilities_it_lists() {
    let config = json(&crate_dir().join("tauri.conf.json"));
    let listed = strings(&config["app"]["security"]["capabilities"]);
    assert_eq!(
        listed,
        ["main"],
        "list capabilities by name; none by default"
    );
    assert_eq!(config["app"]["withGlobalTauri"], Value::Bool(false));
    assert_eq!(
        config["app"]["windows"],
        Value::Array(vec![]),
        "windows are built in code"
    );
}

fn invoke(cmd: &str, origin: &str) -> InvokeRequest {
    InvokeRequest {
        cmd: cmd.to_owned(),
        callback: CallbackFn(0),
        error: CallbackFn(1),
        url: Url::parse(origin).unwrap(),
        body: InvokeBody::default(),
        headers: http::HeaderMap::default(),
        invoke_key: INVOKE_KEY.to_string(),
    }
}

fn app_with_main() -> (tauri::App<MockRuntime>, WebviewWindow<MockRuntime>) {
    let app = puddle_app::with_plugins(mock_builder())
        .invoke_handler(tauri::generate_handler![ping])
        .build(puddle_app::context())
        .unwrap();
    let api = Url::parse("http://127.0.0.1:4711/").unwrap();
    let open: puddle_app::window::OpenExternal = std::sync::Arc::new(|_| {});
    let refused: puddle_app::window::Refused = std::sync::Arc::new(|_| {});
    let window = open_main(&app, &api, "token", None, None, open, refused).unwrap();
    assert_eq!(window.label(), MAIN_LABEL);
    (app, window)
}

fn refusal(result: Result<tauri::ipc::InvokeResponseBody, Value>) -> String {
    match result {
        Err(message) => message.as_str().unwrap_or_default().to_owned(),
        Ok(body) => panic!("the call succeeded: {body:?}"),
    }
}

#[test]
fn an_app_command_is_refused_to_the_main_window_on_every_origin_that_can_reach_it() {
    let (_app, window) = app_with_main();
    // The API origin is what the window loads; the others are what a page could use to look like
    // the app: the webview's own origins on Windows and elsewhere, and a look-alike port.
    for origin in [
        "http://127.0.0.1:4711/",
        "http://127.0.0.1:4712/",
        "http://tauri.localhost/",
        "tauri://localhost/",
        "https://example.org/",
    ] {
        let message = refusal(get_ipc_response(&window, invoke("ping", origin)));
        assert!(message.contains("not allowed"), "{origin}: {message}");
    }
}

#[test]
fn plugin_commands_are_refused_to_the_main_window() {
    let (_app, window) = app_with_main();
    let api = "http://127.0.0.1:4711/";
    for cmd in [
        "plugin:notification|notify",
        "plugin:notification|register_listener",
        "plugin:opener|open_url",
        "plugin:opener|open_path",
        "plugin:window-state|save_window_state",
        "plugin:window|close",
        "plugin:webview|create_webview_window",
        "plugin:event|listen",
        "plugin:app|version",
        "plugin:path|resolve_directory",
        "plugin:fs|read_file",
        "plugin:shell|execute",
        "no_such_command",
    ] {
        let message = refusal(get_ipc_response(&window, invoke(cmd, api)));
        assert!(message.contains("not allowed"), "{cmd}: {message}");
    }
}

mod sandbox_windows {
    use std::sync::Arc;

    use puddle_app::sandbox_window::{
        Hooks, SandboxWindows, Target, WindowSettings, is_sandbox_label, label,
    };
    use puddle_settings::ClipboardRead;
    use puddle_types::WorkspaceName;
    use tauri::Manager;

    use super::*;

    fn open_sandbox(
        app: &tauri::App<MockRuntime>,
        name: &str,
        data: &Path,
    ) -> (WebviewWindow<MockRuntime>, Url) {
        let workspace = WorkspaceName::new(name).unwrap();
        let origin = Url::parse(&format!("http://{name}.localhost:4800/")).unwrap();
        let windows = SandboxWindows::new(Hooks {
            open_external: Arc::new(|_| {}),
            refused: Arc::new(|_| {}),
            new_window: Arc::new(|_, _| {}),
            last_closed: Arc::new(|_| {}),
        });
        let target = Target {
            workspace: workspace.clone(),
            origin: origin.clone(),
            data_folder: data.join(name),
            settings: WindowSettings {
                zoom_hotkeys: true,
                clipboard_read: ClipboardRead::Ask,
            },
            cookie: None,
            debug_port: None,
        };
        let window = windows.open(app, &target).unwrap();
        assert_eq!(window.label(), label(&workspace));
        assert!(is_sandbox_label(window.label()));
        (window, origin)
    }

    #[test]
    fn a_workspace_window_is_refused_every_command_from_its_own_origin_and_the_apps() {
        let (app, _main) = app_with_main();
        let dir = tempfile::tempdir().unwrap();
        let (window, origin) = open_sandbox(&app, "demo", dir.path());
        for from in [
            origin.as_str(),
            "http://127.0.0.1:4711/",
            "http://tauri.localhost/",
            "tauri://localhost/",
            "https://example.org/",
        ] {
            for cmd in [
                "ping",
                "plugin:app|version",
                "plugin:opener|open_url",
                "plugin:opener|open_path",
                "plugin:window|set_title",
                "plugin:webview|create_webview_window",
                "plugin:event|emit",
                "plugin:path|resolve_directory",
                "plugin:window-state|save_window_state",
                "no_such_command",
            ] {
                let message = refusal(get_ipc_response(&window, invoke(cmd, from)));
                assert!(
                    message.contains("not allowed"),
                    "{cmd} from {from}: {message}"
                );
            }
        }
    }

    #[test]
    fn no_capability_names_a_workspace_window() {
        for file in capability_files() {
            let capability = json(&file);
            for label in strings(&capability["windows"])
                .into_iter()
                .chain(strings(&capability["webviews"]))
            {
                assert!(
                    !is_sandbox_label(label),
                    "{}: names {label}",
                    file.display()
                );
            }
        }
    }

    #[test]
    fn a_workspace_window_exists_once_and_has_its_own_data_folder() {
        let (app, _main) = app_with_main();
        let dir = tempfile::tempdir().unwrap();
        let (_first, _) = open_sandbox(&app, "alpha", dir.path());
        let (_second, _) = open_sandbox(&app, "beta", dir.path());
        assert!(dir.path().join("alpha").is_dir() && dir.path().join("beta").is_dir());
        assert_eq!(app.webview_windows().len(), 3);
        // Opening again brings the window forward and builds no second one.
        let (_again, _) = open_sandbox(&app, "alpha", dir.path());
        assert_eq!(app.webview_windows().len(), 3);
    }
}
