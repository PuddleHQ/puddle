// SPDX-License-Identifier: AGPL-3.0-or-later
#![expect(
    clippy::unwrap_used,
    reason = "helpers outside #[test] fns run only in tests; a panic is how a test fails"
)]

use std::sync::Mutex;

use tauri::test::{MockRuntime, mock_builder};

use super::*;
use crate::backend::tests::{fixture_options, get, start_fixture};

fn app() -> tauri::App<MockRuntime> {
    with_plugins(mock_builder()).build(context()).unwrap()
}

#[test]
fn start_opens_the_main_window_on_the_backend_and_stop_ends_the_backend() {
    let app = app();
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("api.json");
    let options = Options {
        connection_file: Some(file.clone()),
        ..fixture_options()
    };
    start(app.handle(), &options, Source::Fixture).unwrap();

    let backend = app.state::<Backend>();
    let window = app.get_webview_window(MAIN_LABEL).unwrap();
    assert_eq!(&window.url().unwrap(), backend.url());
    assert!(file.exists());

    let rt = tokio::runtime::Runtime::new().unwrap();
    let reply = rt.block_on(get(&backend, "/api/pending", Some(backend.token())));
    assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");

    let stopped = stop(app.handle()).unwrap();
    assert_eq!(stopped.steps, [puddle_host::Step::ApiStopped]);
    let port = backend.url().port().unwrap();
    // The port was the system's pick and another test can take it at once, so the check is
    // that the API no longer answers its token, not that nothing accepts.
    assert_ne!(
        puddle_test_kit::http_status(
            ([127, 0, 0, 1], port).into(),
            "/api/pending",
            backend.token()
        ),
        Some(200)
    );
    // The hook can run again (a second Exit event, or no backend at all).
    assert_eq!(stop(app.handle()), None);
}

#[test]
fn quitting_with_an_editor_window_open_asks_its_guest_once_while_the_host_still_runs() {
    let app = app();
    let backend = tauri::async_runtime::block_on(start_fixture(0));
    let port = backend.url().port().unwrap();
    let asked = Arc::new(Mutex::new(Vec::new()));
    let sink = asked.clone();
    let backend = backend.with_extension_hosts(Arc::new(move |workspace| {
        // Whether the host's API still answers when the guest is asked: the cleanup comes
        // before anything is stopped.
        let up = std::net::TcpStream::connect(("127.0.0.1", port)).is_ok();
        sink.lock().unwrap().push((workspace.to_string(), up));
        Box::pin(async { Ok(()) })
    }));
    let problems = backend.problems().clone();
    open(app.handle(), &fixture_options(), backend).unwrap();
    let windows = SandboxWindows::new(sandbox_hooks(
        app.handle(),
        &problems,
        app.state::<Backend>().extension_hosts(),
    ));
    let demo = WorkspaceId::new("demo").unwrap();
    let editor = tauri::WebviewUrl::External("http://127.0.0.1:1/".parse().unwrap());
    tauri::WebviewWindowBuilder::new(app.handle(), "vs-demo", editor)
        .build()
        .unwrap();
    windows.opened(&demo);
    app.manage(windows);

    assert!(stop(app.handle()).is_some());
    assert_eq!(*asked.lock().unwrap(), [("demo".to_owned(), true)]);
    // A second exit event asks nobody again: the real window system may never report the
    // destroyed windows back once the loop has ended.
    assert_eq!(stop(app.handle()), None);
    assert_eq!(asked.lock().unwrap().len(), 1);
}

#[test]
fn a_probe_in_the_options_opens_its_workspace_window_beside_the_main_one() {
    let app = app();
    let dir = tempfile::tempdir().unwrap();
    let options = Options {
        data_dir: Some(dir.path().to_owned()),
        sandbox_probe: SandboxProbe::parse(
            Some("http://probe.localhost:4900/attack.html"),
            Some("9444"),
            None,
            None,
        ),
        ..fixture_options()
    };
    start(app.handle(), &options, Source::Fixture).unwrap();
    assert!(app.get_webview_window("vs-probe").is_some());
    assert!(app.get_webview_window(MAIN_LABEL).is_some());
    assert!(stop(app.handle()).is_some());
}

#[test]
fn stop_without_a_backend_does_nothing() {
    assert_eq!(stop(app().handle()), None);
}

#[test]
fn a_start_at_sign_in_opens_the_window_out_of_the_way() {
    let app = app();
    let options = Options {
        at_login: true,
        ..fixture_options()
    };
    start(app.handle(), &options, Source::Fixture).unwrap();
    assert!(app.get_webview_window(MAIN_LABEL).is_some());
    assert!(stop(app.handle()).is_some());
}

#[test]
fn a_second_start_brings_the_window_forward_unless_it_came_from_the_sign_in_entry() {
    let from_user = ["puddle-app".to_owned()];
    let from_sign_in = ["puddle-app".to_owned(), autostart::AT_LOGIN_FLAG.to_owned()];
    let mut forward = 0;
    bring_forward_unless_sign_in(&from_user, || forward += 1);
    assert_eq!(
        forward, 1,
        "a second start by the user left the window behind"
    );
    bring_forward_unless_sign_in(&from_sign_in, || forward += 1);
    assert_eq!(
        forward, 1,
        "the sign-in entry's start brought the window forward"
    );
}

#[test]
fn a_second_start_reaches_the_main_window_and_is_harmless_without_one() {
    let app = app();
    on_second_start(app.handle(), &["puddle-app".to_owned()]);
    start(app.handle(), &fixture_options(), Source::Fixture).unwrap();
    on_second_start(app.handle(), &["puddle-app".to_owned()]);
    on_second_start(
        app.handle(),
        &["puddle-app".to_owned(), autostart::AT_LOGIN_FLAG.to_owned()],
    );
    assert!(app.get_webview_window(MAIN_LABEL).is_some());
    assert!(stop(app.handle()).is_some());
}

#[test]
fn focusing_without_a_main_window_does_nothing_and_with_one_succeeds() {
    let app = app();
    focus_main(app.handle());
    start(app.handle(), &fixture_options(), Source::Fixture).unwrap();
    focus_main(app.handle());
    assert!(stop(app.handle()).is_some());
}

#[test]
fn a_failed_start_leaves_no_window() {
    let app = app();
    let taken = tauri::async_runtime::block_on(start_fixture(0));
    let options = Options {
        port: taken.url().port().unwrap(),
        ..fixture_options()
    };
    let err = start(app.handle(), &options, Source::Fixture).unwrap_err();
    assert!(matches!(err, ShellError::Backend(_)), "{err}");
    assert!(app.get_webview_window(MAIN_LABEL).is_none());
    tauri::async_runtime::block_on(taken.shutdown());
}

#[test]
fn a_refused_link_reaches_the_pages_problem_list() {
    let app = app();
    start(app.handle(), &fixture_options(), Source::Fixture).unwrap();
    let problems = app.state::<Backend>().problems().clone();
    refuser(&problems)(&Url::parse("file:///etc/passwd").unwrap());
    let listed: Vec<_> = problems
        .list()
        .into_iter()
        .filter(|p| p.key == "app:link")
        .collect();
    assert_eq!(listed.len(), 1);
    assert!(listed[0].title.contains("file:"), "{}", listed[0].title);
    assert!(stop(app.handle()).is_some());
}
