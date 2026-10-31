// SPDX-License-Identifier: GPL-3.0-or-later
//! The shell over the Puddle host: the host prepared the way the program does it, started on a
//! fake sandbox runtime, the main window opened on it (Tauri's mock runtime), and the quit sequence.
#![expect(
    clippy::unwrap_used,
    reason = "helpers outside #[test] fns run only in tests; a panic is how a test fails"
)]

use std::future::Future;
use std::path::Path;
use std::sync::{Arc, Mutex};

use puddle_api::ConnectionInfo;
use puddle_certs::CorporateRoots;
use puddle_certs::devcert::GuestDevCert;
use puddle_compute::fake::FakeRuntime;
use puddle_host::{
    GuestCertSource, HostError, HostOptions, Platform, RuntimeFactory, RuntimeInputs,
    SHUTDOWN_STEPS,
};
use puddle_runtime::{RuntimeLayout, RuntimeVersion};
use puddle_secrets::MemoryStore;
use puddle_upstream::{Discovery, FakeOs, ProxyConfig};
use tauri::test::{MockRuntime, mock_builder};
use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use puddle_app::backend::{Backend, BackendError, Source};
use puddle_app::sandbox_window::SandboxWindows;
use puddle_app::window::MAIN_LABEL;
use puddle_app::{Options, ShellError, context, shell, with_plugins};

/// The machine as the host's synchronous phase sees it: records what it was asked, changes
/// nothing (the process environment stays as the test harness has it).
#[derive(Default)]
struct Machine {
    asked: Mutex<Vec<&'static str>>,
}

impl Machine {
    fn asked(&self) -> Vec<&'static str> {
        self.asked.lock().unwrap().clone()
    }
}

impl Platform for Machine {
    fn check_runtime(&self, _: &RuntimeLayout, _: &RuntimeVersion) -> Result<(), HostError> {
        self.asked.lock().unwrap().push("check_runtime");
        Ok(())
    }

    fn corporate_roots(&self) -> Result<CorporateRoots, HostError> {
        self.asked.lock().unwrap().push("corporate_roots");
        Ok(CorporateRoots::default())
    }
}

/// Opens the fake sandbox runtime in place of msb.
struct FakeFactory(FakeRuntime);

impl RuntimeFactory for FakeFactory {
    type Runtime = FakeRuntime;

    fn open(
        &self,
        _: RuntimeInputs<'_>,
    ) -> impl Future<Output = Result<FakeRuntime, HostError>> + Send {
        std::future::ready(Ok(self.0.clone()))
    }
}

/// No development certificate: the host neither looks in the machine's stores nor makes one.
struct NoDevCert;

impl GuestCertSource for NoDevCert {
    fn guest_cert(&self) -> Option<GuestDevCert> {
        None
    }
}

impl puddle_api::DevCertService for NoDevCert {
    fn status(&self) -> puddle_api::wire::DevCertificate {
        puddle_api::wire::DevCertificate::NotChecked
    }

    fn trust(&self) -> puddle_api::wire::DevCertTrustResult {
        puddle_api::wire::DevCertTrustResult {
            dev_certificate: puddle_api::wire::DevCertificate::NotChecked,
            detail: None,
        }
    }
}

/// What the host takes in a test: no system proxy settings, no credential store, no certificate
/// stores of this machine.
fn hermetic() -> HostOptions {
    let mut options = HostOptions::default();
    options.discovery = Some(Discovery::new(
        FakeOs::new(ProxyConfig::default()),
        puddle_upstream::Config::default(),
    ));
    options.secret_store = Some(Arc::new(MemoryStore::new()));
    options.dev_cert = Some(Arc::new(NoDevCert));
    options
}

/// The options of a run over its own data folder, with a stand-in guest agent (no machine boots).
fn options_in(dir: &Path) -> Options {
    let agent = dir.join("puddle-agent");
    std::fs::write(&agent, b"#!agent\n").unwrap();
    Options {
        data_dir: Some(dir.join("data")),
        agent: Some(agent),
        ..Options::default()
    }
}

fn start(options: &Options, machine: &Machine) -> Result<Backend, BackendError> {
    let source = Source::prepare(options, machine);
    tauri::async_runtime::block_on(source.start_with(
        options,
        &FakeFactory(FakeRuntime::new()),
        hermetic(),
    ))
}

fn app() -> tauri::App<MockRuntime> {
    with_plugins(mock_builder()).build(context()).unwrap()
}

/// One HTTP/1.1 GET with the token; returns the status code and the body.
fn get(url: &url::Url, token: &str, path: &str) -> (u16, String) {
    tauri::async_runtime::block_on(async {
        let host = format!("127.0.0.1:{}", url.port().unwrap());
        let mut stream = TcpStream::connect(&host).await.unwrap();
        let request = format!(
            "GET {path} HTTP/1.1\r\nHost: {host}\r\nAuthorization: Bearer {token}\r\nConnection: close\r\n\r\n"
        );
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut reply = String::new();
        stream.read_to_string(&mut reply).await.unwrap();
        let (head, body) = reply.split_once("\r\n\r\n").unwrap();
        let status = head.split(' ').nth(1).unwrap().parse().unwrap();
        (status, body.to_owned())
    })
}

#[test]
fn the_window_opens_on_the_hosts_api_with_its_token_and_the_system_check_answers() {
    let dir = tempfile::tempdir().unwrap();
    let options = options_in(dir.path());
    let machine = Machine::default();
    let backend = start(&options, &machine).unwrap();
    assert_eq!(machine.asked(), ["check_runtime", "corporate_roots"]);
    let app = app();
    shell::open(app.handle(), &options, backend).unwrap();

    let backend = app.state::<Backend>();
    let window = app.get_webview_window(MAIN_LABEL).unwrap();
    assert_eq!(&window.url().unwrap(), backend.url());
    assert_eq!(backend.url().host_str(), Some("127.0.0.1"));
    // The command line reaches the same host through the connection file in the data folder.
    let file = ConnectionInfo::read(&dir.path().join("data").join("api.json")).unwrap();
    assert_eq!(file.token.expose(), backend.token());

    // The fixture has no system check (503); the host runs `puddle doctor`'s checks.
    let (status, body) = get(backend.url(), backend.token(), "/api/doctor?boot=false");
    assert_eq!(status, 200, "{body}");
    let report: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(
        report["checks"].as_array().is_some_and(|c| !c.is_empty()),
        "{body}"
    );
    let (status, body) = get(backend.url(), backend.token(), "/api/workspaces");
    assert_eq!((status, body.as_str()), (200, r#"{"workspaces":[]}"#));
    shell::stop(app.handle());
}

#[test]
fn a_started_shell_has_the_windows_the_launcher_opens_into() {
    let dir = tempfile::tempdir().unwrap();
    let options = options_in(dir.path());
    let app = app();
    assert!(app.try_state::<SandboxWindows>().is_none());
    let source = Source::prepare(&options, &Machine::default());
    shell::start_with(
        app.handle(),
        &options,
        source,
        &FakeFactory(FakeRuntime::new()),
    )
    .unwrap();
    // The host's own launcher is the shell's: its windows exist and none is open.
    let windows = app.state::<SandboxWindows>();
    assert_eq!(windows.open_count(&"demo".parse().unwrap()), 0);
    shell::stop(app.handle());
}

#[test]
fn quitting_stops_the_host_in_order_and_frees_its_data_folder() {
    let dir = tempfile::tempdir().unwrap();
    let options = options_in(dir.path());
    let app = app();
    shell::open(
        app.handle(),
        &options,
        start(&options, &Machine::default()).unwrap(),
    )
    .unwrap();
    // The mock window system never reports a destroyed window back, so the editor window's end
    // shows only in a real one; here it is the quit's first step.
    let editor = WebviewUrl::External("http://127.0.0.1:1/".parse().unwrap());
    WebviewWindowBuilder::new(app.handle(), "vs-demo", editor)
        .build()
        .unwrap();
    let port = app.state::<Backend>().url().port().unwrap();

    let stopped = shell::stop(app.handle()).unwrap();
    assert_eq!(stopped.steps, SHUTDOWN_STEPS);
    assert_eq!(stopped.operations_ended, 0);
    assert!(std::net::TcpStream::connect(("127.0.0.1", port)).is_err());
    // A second exit event finds nothing left to stop.
    assert_eq!(shell::stop(app.handle()), None);
    // The host let go of the data folder: the next start gets it.
    start(&options, &Machine::default()).unwrap();
}

#[test]
fn the_backend_asks_a_workspace_s_guest_to_end_its_extension_hosts_while_the_host_runs() {
    let dir = tempfile::tempdir().unwrap();
    let options = options_in(dir.path());
    let backend = start(&options, &Machine::default()).unwrap();
    let ends = backend.extension_hosts();
    let demo = puddle_types::WorkspaceName::new("demo").unwrap();

    // No workspace of that name runs: the host says so, in words.
    let reason = tauri::async_runtime::block_on(ends(demo.clone())).unwrap_err();
    assert_eq!(reason, "the workspace is not running");

    // After the shutdown the host is gone and the handle, which does not keep it alive, says so.
    tauri::async_runtime::block_on(backend.shutdown()).unwrap();
    let reason = tauri::async_runtime::block_on(ends(demo)).unwrap_err();
    assert_eq!(reason, "the host has stopped");
    drop(backend);
    // The handle kept nothing: the data folder is free for the next start.
    start(&options, &Machine::default()).unwrap();
}

/// Windows gives a program's main thread 1 MiB of stack by default; `build.rs` links the program
/// with 8 MiB because the host's start-up, unoptimised, outgrows 1 MiB (the setup hook and the exit
/// hook run on that thread). This test only shows the work fits a 1 MiB thread where frames are
/// small; the program's real stack is checked by the Windows smoke test.
#[test]
fn the_host_starts_and_stops_from_a_thread_with_the_windows_main_thread_stack() {
    let dir = tempfile::tempdir().unwrap();
    let options = options_in(dir.path());
    let app = app();
    let handle = app.handle().clone();
    let source = Source::prepare(&options, &Machine::default());
    let stopped = std::thread::Builder::new()
        .stack_size(1024 * 1024)
        .spawn(move || {
            shell::start(&handle, &options, source).unwrap();
            shell::stop(&handle)
        })
        .unwrap()
        .join()
        .unwrap();
    assert_eq!(stopped.unwrap().steps, SHUTDOWN_STEPS);
}

#[test]
fn a_second_shell_on_the_same_data_folder_says_why_and_opens_no_window() {
    let dir = tempfile::tempdir().unwrap();
    let options = options_in(dir.path());
    let first = start(&options, &Machine::default()).unwrap();

    let app = app();
    let machine = Machine::default();
    let err =
        shell::start(app.handle(), &options, Source::prepare(&options, &machine)).unwrap_err();
    assert!(
        matches!(
            err,
            ShellError::Backend(BackendError::Host(HostError::DataFolder(_)))
        ),
        "{err}"
    );
    assert_eq!(machine.asked(), Vec::<&str>::new());
    assert!(app.get_webview_window(MAIN_LABEL).is_none());
    tauri::async_runtime::block_on(first.shutdown());
}

#[cfg(feature = "fixture")]
#[test]
fn the_fixture_never_asks_the_machine() {
    let machine = Machine::default();
    let options = Options {
        fixture: true,
        ..Options::default()
    };
    assert!(matches!(
        Source::prepare(&options, &machine),
        Source::Fixture
    ));
    assert_eq!(machine.asked(), Vec::<&str>::new());
}

#[cfg(not(feature = "fixture"))]
#[test]
fn a_build_without_the_fixture_refuses_to_show_it() {
    let options = Options {
        fixture: true,
        ..Options::default()
    };
    let err = tauri::async_runtime::block_on(Source::Fixture.start(&options)).unwrap_err();
    assert!(matches!(err, BackendError::NoFixture), "{err}");
}
