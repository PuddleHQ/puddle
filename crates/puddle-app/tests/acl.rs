// SPDX-License-Identifier: AGPL-3.0-or-later
//! The main window has one Tauri permission: the folder dialog.
//!
//! Two layers: the capability files say it (static), and Tauri's own authority enforces it
//! (an IPC call that is not the folder dialog, or that comes from another window or another
//! origin, is refused). A change that grants `main` anything else, or any other window the
//! dialog, fails here, on purpose: it needs a decision, not a drive-by.

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

/// The one permission the main window holds: the native folder dialog the create form's
/// "Choose folder" button opens.
const MAIN_PERMISSIONS: [&str; 1] = ["dialog:allow-open"];

/// The page is served from the API on a loopback port picked at start-up, so the capability
/// names loopback with any port. The window itself never leaves the API's origin.
const MAIN_REMOTE_URLS: [&str; 1] = ["http://127.0.0.1:*"];

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
fn no_capability_grants_the_main_window_more_than_the_folder_dialog_or_reaches_another_origin() {
    for file in capability_files() {
        let capability = json(&file);
        let name = file.display();
        let windows = strings(&capability["windows"]);
        let webviews = strings(&capability["webviews"]);
        for label in windows.iter().chain(webviews.iter()) {
            assert!(!label.contains('*'), "{name}: wildcard label {label:?}");
        }
        let covers_main = windows.contains(&MAIN_LABEL) || webviews.contains(&MAIN_LABEL);
        if covers_main {
            assert_eq!(
                strings(&capability["permissions"]),
                MAIN_PERMISSIONS,
                "{name}: the main window gets the folder dialog and nothing else"
            );
            // The SPA is a remote page to Tauri, so the permission needs a `remote` entry; it is
            // limited to the API's loopback origin.
            assert_eq!(
                strings(&capability["remote"]["urls"]),
                MAIN_REMOTE_URLS,
                "{name}: the main window's remote origin"
            );
        } else {
            assert!(
                capability.get("remote").is_none(),
                "{name}: `remote` is only for the main window's folder dialog"
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
        "plugin:dialog|save",
        "plugin:dialog|message",
        "plugin:dialog|ask",
        "plugin:dialog|confirm",
        "no_such_command",
    ] {
        let message = refusal(get_ipc_response(&window, invoke(cmd, api)));
        assert!(message.contains("not allowed"), "{cmd}: {message}");
    }
}

#[test]
fn the_folder_dialog_is_allowed_to_the_main_window_on_the_api_origin_only() {
    let (_app, window) = app_with_main();
    // Arguments are missing on purpose: an allowed call fails on them before any dialog opens,
    // so what the test sees is "not allowed" (refused) or something else (let through).
    let allowed = refusal(get_ipc_response(
        &window,
        invoke("plugin:dialog|open", "http://127.0.0.1:4711/"),
    ));
    assert!(!allowed.contains("not allowed"), "{allowed}");
    // Not listed: `tauri://localhost`, the app's own asset origin. Tauri treats a capability
    // with `remote` as covering local origins too; the window never navigates there (its
    // navigation stays on the API's origin) and the app serves no page from it.
    for origin in [
        "http://127.0.0.2:4711/",
        "http://localhost:4711/",
        "https://example.org/",
        "https://127.0.0.1:4711/",
    ] {
        let message = refusal(get_ipc_response(
            &window,
            invoke("plugin:dialog|open", origin),
        ));
        assert!(message.contains("not allowed"), "{origin}: {message}");
    }
    // `http://tauri.localhost` is the app's own asset origin on Windows, the same local origin as
    // `tauri://localhost` elsewhere: Tauri lets the capability cover it on Windows only. The
    // window never navigates there, so the test pins what Tauri does instead of hiding the case.
    let message = refusal(get_ipc_response(
        &window,
        invoke("plugin:dialog|open", "http://tauri.localhost/"),
    ));
    assert_eq!(
        message.contains("not allowed"),
        !cfg!(windows),
        "http://tauri.localhost/: {message}"
    );
}

mod sandbox_windows {
    use std::sync::Arc;

    use puddle_app::sandbox_window::{
        Hooks, SandboxWindows, Target, WindowSettings, is_sandbox_label, label,
    };
    use puddle_settings::ClipboardRead;
    use puddle_types::WorkspaceId;
    use tauri::Manager;

    use super::*;

    fn open_sandbox(
        app: &tauri::App<MockRuntime>,
        name: &str,
        data: &Path,
    ) -> (WebviewWindow<MockRuntime>, Url) {
        let workspace = WorkspaceId::new(name).unwrap();
        let origin = Url::parse(&format!("http://{name}.localhost:4800/")).unwrap();
        let windows = SandboxWindows::new(Hooks {
            open_external: Arc::new(|_| {}),
            refused: Arc::new(|_| {}),
            new_window: Arc::new(|_, _| {}),
            last_closed: Arc::new(|_| {}),
        });
        let target = Target {
            workspace: workspace.clone(),
            display_name: name.to_owned(),
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
                "plugin:dialog|open",
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
