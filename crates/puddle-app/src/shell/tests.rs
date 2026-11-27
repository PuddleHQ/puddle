// SPDX-License-Identifier: AGPL-3.0-or-later
use puddle_lifecycle::{ShutdownReport, StopOutcome, TrimOutcome, WorkspaceOutcome};
use puddle_types::SandboxName;
use std::sync::Mutex;

use super::*;
use crate::backend::EndExtensionHostsFailed;
use tracing::Level;

#[test]
fn puddle_log_wins_over_rust_log_and_scopes_the_level_to_puddle_crates() {
    let (filter, said) = log_filter(Some("trace"), Some("error"));
    assert!(said.is_none());
    assert!(filter.would_enable("puddle_app", &Level::TRACE));
    assert!(!filter.would_enable("tauri", &Level::DEBUG));
}

#[test]
fn rust_log_keeps_dependencies_at_the_ceiling_unless_it_names_them() {
    let (filter, said) = log_filter(None, Some("trace"));
    assert!(said.is_none());
    assert!(filter.would_enable("puddle_app", &Level::TRACE));
    for dep in ["hyper", "rustls", "h2", "russh", "tauri"] {
        assert!(filter.would_enable(dep, &Level::INFO), "{dep}");
        assert!(!filter.would_enable(dep, &Level::DEBUG), "{dep}");
    }
    let (filter, _) = log_filter(None, Some("warn,puddle_host=trace,hyper=debug"));
    assert!(filter.would_enable("puddle_host", &Level::TRACE));
    assert!(!filter.would_enable("puddle_app", &Level::INFO));
    assert!(filter.would_enable("hyper", &Level::DEBUG));
    assert!(!filter.would_enable("rustls", &Level::INFO));
}

#[test]
fn a_puddle_log_that_does_not_parse_gives_info_and_comes_back() {
    let (filter, said) = log_filter(Some("loud"), Some("trace"));
    assert!(said.unwrap().starts_with("PUDDLE_LOG=\"loud\""));
    assert!(filter.would_enable("puddle_app", &Level::INFO));
    assert!(!filter.would_enable("puddle_app", &Level::DEBUG));
}

#[test]
fn a_rust_log_that_does_not_parse_gives_info_and_comes_back() {
    let (filter, said) = log_filter(None, Some("puddle=loud"));
    assert!(said.unwrap().starts_with("RUST_LOG=\"puddle=loud\""));
    assert!(filter.would_enable("puddle_app", &Level::INFO));
    assert!(!filter.would_enable("puddle_app", &Level::DEBUG));
}

#[test]
fn a_clean_quit_logs_no_warning() {
    let outcome = WorkspaceOutcome {
        sandbox: SandboxName::new("fine").unwrap(),
        trim: TrimOutcome::Trimmed,
        stop: StopOutcome::Stopped,
    };
    let clean = HostShutdown {
        sandboxes: ShutdownReport {
            sandboxes: vec![outcome],
        },
        ..HostShutdown::default()
    };
    assert_eq!(quit_warnings(&clean), Vec::<String>::new());
}

#[test]
fn the_quit_names_each_workspace_that_did_not_trim_or_stop_and_the_ended_operations() {
    let shutdown = HostShutdown {
        sandboxes: ShutdownReport {
            sandboxes: vec![WorkspaceOutcome {
                sandbox: SandboxName::new("slow").unwrap(),
                trim: TrimOutcome::Error("no answer".to_owned()),
                stop: StopOutcome::Forced,
            }],
        },
        operations_ended: 2,
        ..HostShutdown::default()
    };
    assert_eq!(
        quit_warnings(&shutdown),
        [
            "the disk of slow was not trimmed: no answer; Reclaim space trims it later",
            "slow did not stop cleanly: it did not shut down in time and was ended by force; files it had not written yet may be missing",
            "2 workspace operations were still running and were ended",
        ]
    );
}

#[test]
fn only_an_installed_build_manages_the_sign_in_entry() {
    assert!(manages_login(false));
    assert!(!manages_login(true));
    // Whatever the environment says, a build under test never gets the system's entry.
    assert!(Options::default().login.is_none());
}

#[test]
fn the_data_folder_is_the_one_named_else_the_per_user_one() {
    let named = Options {
        data_dir: Some(PathBuf::from("named")),
        ..Options::default()
    };
    assert_eq!(data_folder(&named), Some(PathBuf::from("named")));
    // Only resolved, never created or written.
    let _ = data_folder(&Options::default());
}

/// A way to end extension hosts that records the workspaces it was asked about and refuses
/// the one named `stopped`, as a workspace with no connected agent does.
fn recording_ends() -> (EndExtensionHosts, Arc<Mutex<Vec<String>>>) {
    let asked = Arc::new(Mutex::new(Vec::new()));
    let sink = asked.clone();
    let ends: EndExtensionHosts = Arc::new(move |workspace| {
        sink.lock().unwrap().push(workspace.to_string());
        Box::pin(async move {
            if workspace.as_str() == "stopped" {
                Err(EndExtensionHostsFailed::Guest(
                    puddle_host::EndExtensionHostsError::NotRunning,
                ))
            } else {
                Ok(())
            }
        })
    });
    (ends, asked)
}

fn wait_for(asked: &Arc<Mutex<Vec<String>>>, count: usize) -> Vec<String> {
    puddle_test_kit::eventually_blocking(
        "the guest is asked to end the extension hosts",
        std::time::Duration::from_secs(10),
        || {
            let seen = asked.lock().unwrap().clone();
            (seen.len() >= count).then_some(seen)
        },
    )
}

#[test]
fn the_last_window_closing_asks_the_guest_to_end_the_extension_hosts_and_is_no_problem() {
    let app = tauri::test::mock_app();
    let problems = Arc::new(Problems::new(Arc::new(puddle_api::EventHub::default())));
    let (ends, asked) = recording_ends();
    let hooks = sandbox_hooks(app.handle(), &problems, ends);
    (hooks.last_closed)(&WorkspaceId::new("demo").unwrap());
    // A workspace that has stopped has nothing to end: not the user's concern either.
    (hooks.last_closed)(&WorkspaceId::new("stopped").unwrap());
    // Each ask runs in its own task, so the order is not the calls'.
    let mut seen = wait_for(&asked, 2);
    seen.sort();
    assert_eq!(seen, ["demo", "stopped"]);
    assert_eq!(problems.list().len(), 0, "{:?}", problems.list());
}

#[tokio::test]
async fn the_quit_asks_every_workspace_at_once_and_goes_on_past_a_refusal() {
    let (ends, asked) = recording_ends();
    let names = ["demo", "stopped", "other"].map(|n| WorkspaceId::new(n).unwrap());
    end_extension_hosts(&ends, names.to_vec(), std::time::Duration::from_secs(5)).await;
    let mut seen = asked.lock().unwrap().clone();
    seen.sort();
    assert_eq!(seen, ["demo", "other", "stopped"]);
}

#[tokio::test]
async fn a_guest_that_never_answers_holds_the_quit_up_only_for_the_wait() {
    let ends: EndExtensionHosts = Arc::new(|_| Box::pin(std::future::pending()));
    let demo = WorkspaceId::new("demo").unwrap();
    let wait = std::time::Duration::from_millis(50);
    let before = tokio::time::Instant::now();
    end_extension_hosts(&ends, vec![demo], wait).await;
    assert!(before.elapsed() >= wait);
}

#[test]
fn closing_a_popup_asks_nothing_and_the_last_window_asks_once() {
    let app = tauri::test::mock_app();
    let problems = Arc::new(Problems::new(Arc::new(puddle_api::EventHub::default())));
    let (ends, asked) = recording_ends();
    let windows = SandboxWindows::new(sandbox_hooks(app.handle(), &problems, ends));
    let data = tempfile::tempdir().unwrap();
    let demo = WorkspaceId::new("demo").unwrap();
    let target = Target {
        origin: "http://demo.localhost:4800/".parse().unwrap(),
        data_folder: sandbox_window::data_folder(data.path(), &demo),
        workspace: demo.clone(),
        display_name: "demo".to_owned(),
        settings: WindowSettings {
            zoom_hotkeys: true,
            clipboard_read: ClipboardRead::Ask,
        },
        cookie: None,
        debug_port: None,
    };
    windows.open(app.handle(), &target).unwrap();
    // A second window of the same workspace (the editor's "New Window"), counted as the
    // window system does.
    windows.opened(&demo);
    let ended = windows.on_event(demo);
    ended(&tauri::WindowEvent::Destroyed);
    // An absence window: a call for the first window's end would have arrived by now.
    std::thread::sleep(std::time::Duration::from_millis(100));
    assert!(asked.lock().unwrap().is_empty(), "a window is still open");
    ended(&tauri::WindowEvent::Destroyed);
    assert_eq!(wait_for(&asked, 1), ["demo"]);
}

#[test]
fn the_probe_window_opens_on_its_page_in_a_data_folder_of_its_own() {
    let app = tauri::test::mock_app();
    let data = tempfile::tempdir().unwrap();
    let probe = SandboxProbe {
        workspace: WorkspaceId::new("probe").unwrap(),
        url: Url::parse("http://probe.localhost:4900/attack.html").unwrap(),
        debug_port: 9444,
        forget: false,
        clipboard_read: ClipboardRead::Ask,
    };
    open_probe(app.handle(), &probe, data.path()).unwrap();
    let window = app.get_webview_window("vs-probe").unwrap();
    assert_eq!(window.url().unwrap(), probe.url);
    assert!(data.path().join("webview").join("probe").is_dir());
    assert!(app.try_state::<SandboxWindows>().is_some());
}

#[test]
fn the_probe_needs_a_page_and_a_debug_port() {
    let page = Some("http://probe.localhost:4900/attack.html");
    let probe = SandboxProbe::parse(page, Some("9444"), None, None, None).unwrap();
    assert_eq!((probe.url.path(), probe.debug_port), ("/attack.html", 9444));
    assert_eq!(
        SandboxProbe::parse(None, Some("9444"), None, None, None),
        None
    );
    assert_eq!(SandboxProbe::parse(page, None, None, None, None), None);
    assert_eq!(
        SandboxProbe::parse(Some("not a url"), Some("9444"), None, None, None),
        None
    );
    assert_eq!(
        SandboxProbe::parse(page, Some("high"), None, None, None),
        None
    );
    // The environment is shared between tests: only a run with neither value set shows this.
    assert!(SandboxProbe::from_env().is_none());
}

#[test]
fn the_probe_opens_first_only_under_webdriver_automation() {
    assert!(probe_goes_first(Some("true")));
    for other in [None, Some("false"), Some("1"), Some("")] {
        assert!(!probe_goes_first(other), "{other:?}");
    }
}

#[test]
fn the_probe_stands_for_the_workspace_the_environment_names_and_forgets_only_when_told_to() {
    let page = Some("http://k3x9q2ab.localhost:4900/");
    let named = SandboxProbe::parse(page, Some("9444"), Some("k3x9q2ab"), None, None).unwrap();
    assert_eq!(named.workspace.as_str(), "k3x9q2ab");
    assert!(!named.forget);
    assert_eq!(
        SandboxProbe::parse(page, Some("9444"), None, None, None)
            .unwrap()
            .workspace
            .as_str(),
        "probe"
    );
    // A name that is no workspace id is no probe: it must not quietly open `probe` instead.
    assert_eq!(
        SandboxProbe::parse(page, Some("9444"), Some("Not An Id"), None, None),
        None
    );
    let forgetting = |value| {
        SandboxProbe::parse(page, Some("9444"), None, value, None)
            .unwrap()
            .forget
    };
    assert!(forgetting(Some("1")));
    assert!(!forgetting(Some("0")) && !forgetting(Some("yes")) && !forgetting(None));
}

#[test]
fn a_forgetting_probe_removes_the_workspace_s_webview_data_before_it_opens() {
    let app = tauri::test::mock_app();
    let data = tempfile::tempdir().unwrap();
    let webview = data.path().join("webview");
    let leftovers = [webview.join("probe"), webview.join("probe.forward")];
    for folder in &leftovers {
        std::fs::create_dir_all(folder).unwrap();
        std::fs::write(folder.join("Cookies"), b"old").unwrap();
    }
    let probe = SandboxProbe::parse(
        Some("http://probe.localhost:4900/"),
        Some("9444"),
        None,
        Some("1"),
        None,
    )
    .unwrap();
    let log = logged(|| open_probe(app.handle(), &probe, data.path()).unwrap());
    assert!(log.contains("sandbox window: data removed"), "{log}");
    for folder in &leftovers {
        assert!(!folder.join("Cookies").exists(), "{folder:?} kept its data");
    }
}

#[test]
fn a_forgetting_probe_that_cannot_remove_the_data_says_so_and_then_fails_to_open_over_it() {
    let app = tauri::test::mock_app();
    let data = tempfile::tempdir().unwrap();
    let webview = data.path().join("webview");
    std::fs::create_dir_all(&webview).unwrap();
    // A file where the folder should be: removing it as a folder fails, which is not "absent".
    std::fs::write(webview.join("probe"), b"not a folder").unwrap();
    let probe = SandboxProbe::parse(
        Some("http://probe.localhost:4900/"),
        Some("9444"),
        None,
        Some("1"),
        None,
    )
    .unwrap();
    let mut opened = None;
    let log = logged(|| opened = Some(open_probe(app.handle(), &probe, data.path())));
    assert!(log.contains("sandbox window: data not removed"), "{log}");
    // The window cannot be made over the file that is in its data folder's place: the error comes
    // back, it is not swallowed, and no window set is kept.
    assert!(
        matches!(opened, Some(Err(_))),
        "the open reports its failure: {opened:?}"
    );
    assert!(app.try_state::<SandboxWindows>().is_none());
}

/// Runs `act` under a subscriber that keeps what it logs.
fn logged(act: impl FnOnce()) -> String {
    use std::sync::Mutex;

    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for Capture {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let capture = Capture::default();
    let writer = capture.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || writer.clone())
        .with_ansi(false)
        .finish();
    tracing::subscriber::with_default(subscriber, act);
    std::io::Write::flush(&mut capture.clone()).unwrap();
    let bytes = capture.0.lock().unwrap().clone();
    String::from_utf8(bytes).unwrap()
}

#[test]
fn init_logging_says_once_that_a_bad_filter_gave_info_and_stays_quiet_for_a_good_one() {
    let bad = logged(|| init_logging(Some("loud"), None));
    assert_eq!(
        bad.matches("PUDDLE_LOG=\"loud\" is not a log filter; logging at info")
            .count(),
        1,
        "{bad}"
    );
    let bad_rust = logged(|| init_logging(None, Some("puddle=loud")));
    assert!(
        bad_rust.contains("RUST_LOG=\"puddle=loud\" is not a log filter"),
        "{bad_rust}"
    );
    assert_eq!(logged(|| init_logging(None, None)), "");
    assert_eq!(logged(|| init_logging(Some("debug"), Some("loud"))), "");
}

#[test]
fn init_logging_reads_puddle_log_and_rust_log_by_their_names() {
    let vars = |puddle: Option<&'static str>, rust: Option<&'static str>| {
        move |name: &str| match name {
            "PUDDLE_LOG" => puddle.map(str::to_owned),
            "RUST_LOG" => rust.map(str::to_owned),
            other => panic!("read {other}"),
        }
    };
    let puddle = logged(|| init_logging_from(vars(Some("loud"), None)));
    assert!(
        puddle.contains("PUDDLE_LOG=\"loud\" is not a log filter"),
        "{puddle}"
    );
    let rust = logged(|| init_logging_from(vars(None, Some("puddle=loud"))));
    assert!(
        rust.contains("RUST_LOG=\"puddle=loud\" is not a log filter"),
        "{rust}"
    );
    assert_eq!(logged(|| init_logging_from(vars(None, None))), "");
    // The process's own variables are only read: whatever they hold, the call does not panic.
    let _ = logged(init_logging_from_env);
}

#[test]
fn the_probe_window_logs_each_of_its_links_and_events() {
    let hooks = probe_hooks();
    let url = Url::parse("https://example.org/x").unwrap();
    let probe = WorkspaceId::new("probe").unwrap();
    let log = logged(|| {
        (hooks.open_external)(&url);
        (hooks.refused)(&url);
        (hooks.new_window)(&probe, Err("no room"));
        (hooks.last_closed)(&probe);
    });
    for line in [
        "sandbox window: opened in the browser url=https://example.org/x",
        "sandbox window: link refused url=https://example.org/x",
        "sandbox window: new window workspace=probe end=Err(\"no room\")",
        "sandbox window: last window closed workspace=probe",
    ] {
        assert!(log.contains(line), "missing {line:?} in {log}");
    }
}

#[test]
fn a_failed_new_window_reaches_the_pages_problem_list_and_a_later_one_withdraws_it() {
    let problems = Arc::new(Problems::new(Arc::new(puddle_api::EventHub::default())));
    let demo = WorkspaceId::new("demo").unwrap();
    let report = window_reporter(&problems);
    let window_problems = || {
        problems
            .list()
            .into_iter()
            .filter(|p| p.key.starts_with("app:window"))
            .collect::<Vec<_>>()
    };
    report(&demo, Err("the window system said no"));
    let listed = window_problems();
    assert_eq!(listed.len(), 1);
    assert!(listed[0].title.contains("demo"));
    assert!(listed[0].detail.contains("the window system said no"));
    report(&demo, Ok(()));
    assert_eq!(window_problems(), []);
}

#[test]
fn the_defaults_pick_a_free_port_and_ask_for_nothing() {
    let options = Options::default();
    assert_eq!(options.port, 0);
    assert!(options.connection_file.is_none() && options.dev_url.is_none());
    assert!(options.debug_port.is_none() && !options.fixture);
    assert!(options.data_dir.is_none() && options.agent.is_none());
    assert!(options.sandbox_probe.is_none());
    assert_eq!(Options::from_env().port, 0);
}

#[test]
fn the_editor_windows_take_the_debug_port_after_the_main_window_s() {
    assert_eq!(editor_debug_port(Some(9340)), Some(9341));
    assert_eq!(editor_debug_port(None), None);
    assert_eq!(editor_debug_port(Some(u16::MAX)), None);
}

#[test]
fn a_start_up_failure_is_recorded_in_the_data_folder_and_named_in_the_message() {
    let dir = tempfile::tempdir().unwrap();
    let options = Options {
        data_dir: Some(dir.path().to_owned()),
        ..Options::default()
    };
    let err = ShellError::Containment(LifecycleError::Job {
        op: "assign",
        source: std::io::Error::other("denied"),
    });
    let message = report_startup_failure(&options, &err);
    let file = dir.path().join(crash::STARTUP_ERROR_FILE);
    assert!(
        message.contains("cannot start: job object: assign failed"),
        "{message}"
    );
    assert!(message.contains(&file.display().to_string()), "{message}");
    let text = std::fs::read_to_string(file).unwrap();
    assert!(
        text.contains("cannot start: job object: assign failed"),
        "{text}"
    );
}

#[test]
fn a_start_up_failure_lands_in_the_fallback_folder_when_the_data_folder_cannot_be_written() {
    let dir = tempfile::tempdir().unwrap();
    // a file where the data folder should be: it can be neither locked nor created
    let blocker = dir.path().join("blocker");
    std::fs::write(&blocker, "").unwrap();
    let fallback = dir.path().join("fallback");
    let written = crash::write_startup_error_or_fallback(
        Some(&blocker.join("data")),
        &fallback,
        "the backend didn't start",
    )
    .unwrap();
    assert_eq!(written, fallback.join(crash::STARTUP_ERROR_FILE));
    assert!(
        std::fs::read_to_string(written)
            .unwrap()
            .contains("the backend didn't start")
    );
}

#[test]
fn a_setup_failure_the_hook_stored_is_what_run_returns_after_the_loop_ends() {
    let stored = Mutex::new(Some(ShellError::Containment(LifecycleError::Job {
        op: "assign",
        source: std::io::Error::other("denied"),
    })));
    assert!(matches!(
        setup_outcome(&stored),
        Err(ShellError::Containment(_))
    ));
    assert!(setup_outcome(&stored).is_ok());
}

#[test]
fn a_failure_the_setup_hook_stored_is_what_run_returns_and_tauri_s_own_otherwise() {
    let stored = Mutex::new(Some(ShellError::Containment(LifecycleError::Job {
        op: "assign",
        source: std::io::Error::other("denied"),
    })));
    let tauri_error = || tauri::Error::InvalidWebviewUrl("x");
    assert!(matches!(
        build_failure(&stored, tauri_error()),
        ShellError::Containment(_)
    ));
    // Taken once: a second failure is Tauri's.
    assert!(matches!(
        build_failure(&stored, tauri_error()),
        ShellError::Tauri(_)
    ));
}

#[test]
fn the_installer_refuses_to_replace_a_newer_version_with_an_older_one() {
    // `bundle.windows.allowDowngrades` is what makes the WiX template write the
    // DowngradeErrorMessage; Tauri's default (true) lets an older package install over a newer
    // one. The compiled-in context leaves the bundle section out, so the file itself is read.
    // ci/msi-install-test.ps1 runs the refusal on a real package.
    let config: serde_json::Value =
        serde_json::from_str(include_str!("../../tauri.conf.json")).unwrap();
    assert_eq!(
        config["bundle"]["windows"]["allowDowngrades"],
        serde_json::Value::Bool(false)
    );
}

#[test]
fn the_probe_opens_with_the_clipboard_setting_the_environment_names_and_refuses_a_typo() {
    let page = Some("http://probe.localhost:4900/");
    let with = |value| SandboxProbe::parse(page, Some("9444"), None, None, value);
    assert_eq!(with(None).unwrap().clipboard_read, ClipboardRead::Allow);
    assert_eq!(
        with(Some("ask")).unwrap().clipboard_read,
        ClipboardRead::Ask
    );
    assert_eq!(
        with(Some("allow")).unwrap().clipboard_read,
        ClipboardRead::Allow
    );
    assert_eq!(
        with(Some("deny")).unwrap().clipboard_read,
        ClipboardRead::Deny
    );
    assert_eq!(with(Some("Deny")), None);
}
