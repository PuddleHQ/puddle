// SPDX-License-Identifier: GPL-3.0-or-later
//! Wiring: plugins, the backend and the main window, and the exit hook (the quit sequence).

use std::path::PathBuf;
use std::sync::Arc;

use puddle_host::{HostOptions, HostShutdown, MsbFactory, RuntimeFactory, SystemPlatform};
use puddle_lifecycle::LifecycleError;
use puddle_types::Problems;
use tauri::{AppHandle, Manager, RunEvent, Runtime};
use tauri_plugin_opener::OpenerExt;
use url::Url;

use crate::backend::{Backend, BackendError, EndExtensionHosts, Source};
use crate::launcher::AppLauncher;
use puddle_settings::ClipboardRead;
use puddle_types::WorkspaceName;

use crate::sandbox_window::{self, Hooks, NewWindowReport, SandboxWindows, Target, WindowSettings};
use crate::window::{MAIN_LABEL, OpenExternal, Refused, open_main};
use crate::{crash, desktop, links};

/// The folder under the data folder that holds each workspace window's own data folder.
const WEBVIEW_FOLDER: &str = "webview";

/// Why the shell didn't start or ended badly.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ShellError {
    /// The in-process backend didn't start.
    #[error("the backend didn't start: {0}")]
    Backend(#[from] BackendError),
    /// Tauri failed.
    #[error("the window system failed: {0}")]
    Tauri(#[from] tauri::Error),
    /// The shell couldn't put itself in the job that ends the workspaces' machines with it.
    #[error("cannot start: {0}")]
    Containment(#[from] LifecycleError),
}

/// A workspace window for the smoke test to attack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxProbe {
    /// The workspace the window stands for (`probe`).
    pub workspace: WorkspaceName,
    /// The page to open, on a `<workspace>.localhost:<port>` origin.
    pub url: Url,
    /// WebView2's DevTools Protocol port for that window (Windows).
    pub debug_port: u16,
}

impl SandboxProbe {
    /// The probe the environment asks for: `PUDDLE_APP_SANDBOX_PROBE` is the page and
    /// `PUDDLE_APP_SANDBOX_DEBUG_PORT` its debug port.
    fn from_env() -> Option<Self> {
        Self::parse(
            std::env::var("PUDDLE_APP_SANDBOX_PROBE").ok().as_deref(),
            std::env::var("PUDDLE_APP_SANDBOX_DEBUG_PORT")
                .ok()
                .as_deref(),
        )
    }

    /// The probe for these two values; none unless both are there and valid.
    fn parse(url: Option<&str>, debug_port: Option<&str>) -> Option<Self> {
        Some(Self {
            workspace: WorkspaceName::new("probe").ok()?,
            url: Url::parse(url?).ok()?,
            debug_port: debug_port?.parse().ok()?,
        })
    }
}

/// How a run differs from the defaults. Development conveniences only; a release build ignores
/// the environment.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Port for the API on `127.0.0.1`; 0 picks a free one.
    pub port: u16,
    /// Where to write the connection file (`PUDDLE_CONNECTION_FILE`), so `npm run dev` in `ui/`
    /// can proxy to this backend.
    pub connection_file: Option<PathBuf>,
    /// A development server for the page (`PUDDLE_APP_UI_URL`, e.g. `http://localhost:5173/`),
    /// shown in the window instead of the API's own page. The token is handed to it as well.
    pub dev_url: Option<Url>,
    /// A port for WebView2's DevTools Protocol (`PUDDLE_APP_DEBUG_PORT`), which the smoke test
    /// drives the window through. Windows only.
    pub debug_port: Option<u16>,
    /// Show the in-memory fixture instead of running the host (`PUDDLE_APP_FIXTURE=1`; builds with
    /// the `fixture` feature).
    pub fixture: bool,
    /// The host's data folder instead of the per-user one (`PUDDLE_APP_DATA_DIR`), so a test run
    /// or a second developer instance has state of its own.
    pub data_dir: Option<PathBuf>,
    /// A workspace window on a page the test serves (`PUDDLE_APP_SANDBOX_PROBE`), for the smoke
    /// test: it stands in for a workspace's editor, so the window's guards run in a real webview.
    pub sandbox_probe: Option<SandboxProbe>,
    /// The guest agent instead of the one in the runtime folder (`PUDDLE_AGENT_BIN`, as for
    /// `puddle serve`).
    pub agent: Option<PathBuf>,
}

impl Options {
    /// The options the environment asks for (debug builds only).
    #[must_use]
    pub fn from_env() -> Self {
        if cfg!(debug_assertions) {
            Self {
                port: 0,
                connection_file: std::env::var_os("PUDDLE_CONNECTION_FILE").map(PathBuf::from),
                dev_url: std::env::var("PUDDLE_APP_UI_URL")
                    .ok()
                    .and_then(|v| Url::parse(&v).ok()),
                debug_port: std::env::var("PUDDLE_APP_DEBUG_PORT")
                    .ok()
                    .and_then(|v| v.parse().ok()),
                fixture: std::env::var_os("PUDDLE_APP_FIXTURE").is_some_and(|v| v == "1"),
                data_dir: std::env::var_os("PUDDLE_APP_DATA_DIR").map(PathBuf::from),
                sandbox_probe: SandboxProbe::from_env(),
                agent: std::env::var_os("PUDDLE_AGENT_BIN").map(PathBuf::from),
            }
        } else {
            Self::default()
        }
    }
}

/// The data folder the host uses: the one `options` name, else the per-user one.
fn data_folder(options: &Options) -> Option<PathBuf> {
    options
        .data_dir
        .clone()
        .or_else(|| puddle_fs::data_dir().ok())
}

/// Tauri's context for the app: `tauri.conf.json`, the capabilities and the ACL manifest.
#[must_use]
pub fn context<R: Runtime>() -> tauri::Context<R> {
    tauri::generate_context!()
}

/// Adds the plugins every build has (all but single-instance, which only the real app can have:
/// it talks to the desktop's message bus). None of them is granted to any window.
pub fn with_plugins<R: Runtime>(builder: tauri::Builder<R>) -> tauri::Builder<R> {
    builder
        .plugin(
            tauri_plugin_window_state::Builder::default()
                .with_filter(sandbox_window::remembers_place)
                .build(),
        )
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_opener::init())
}

/// Brings the main window to the front (a second launch asks for this).
pub fn focus_main<R: Runtime>(app: &AppHandle<R>) {
    if let Some(window) = app.get_webview_window(MAIN_LABEL) {
        // Both can fail if the window is being closed; there is nothing to do about that.
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// Hands an address to the default browser through the opener plugin; a failure is raised as a
/// problem the page shows.
fn opener<R: Runtime>(app: &AppHandle<R>, problems: &Arc<Problems>) -> OpenExternal {
    let (app, problems) = (app.clone(), problems.clone());
    Arc::new(move |url: &Url| {
        links::open_and_report(&problems, url, |address| {
            app.opener().open_url(address, None::<&str>)
        });
    })
}

/// Tells the page about a link the window refused.
fn refuser(problems: &Arc<Problems>) -> Refused {
    let problems = problems.clone();
    Arc::new(move |url: &Url| links::report_refused(&problems, url))
}

/// Tells the page how a request for another editor window ended.
fn window_reporter(problems: &Arc<Problems>) -> NewWindowReport {
    let problems = problems.clone();
    Arc::new(
        move |workspace: &WorkspaceName, end: Result<(), &str>| match end {
            Ok(()) => links::window_opened(&problems, workspace),
            Err(reason) => links::report_window_failed(&problems, workspace, reason),
        },
    )
}

/// Starts the backend `source` names and opens the main window on it.
///
/// # Errors
///
/// [`ShellError`] if the backend or the window can't start; no window is left open.
pub fn start<R: Runtime>(
    app: &AppHandle<R>,
    options: &Options,
    source: Source,
) -> Result<(), ShellError> {
    start_with(app, options, source, &MsbFactory)
}

/// [`start`] with the host's runtime opened by `factory` (tests open a fake runtime).
///
/// # Errors
///
/// As [`start`].
pub fn start_with<R: Runtime, F: RuntimeFactory>(
    app: &AppHandle<R>,
    options: &Options,
    source: Source,
    factory: &F,
) -> Result<(), ShellError>
where
    F::Runtime: 'static,
{
    // The host holds the launcher from its start; its windows need the backend's problem list,
    // so they are installed once it runs.
    let launcher = match (&source, data_folder(options)) {
        (Source::Host(_), Some(data)) => Some(Arc::new(AppLauncher::new(
            app.clone(),
            data.join(WEBVIEW_FOLDER),
        ))),
        _ => None,
    };
    let mut host = HostOptions::default();
    if let Some(launcher) = &launcher {
        host.launcher = launcher.clone();
    }
    let backend = tauri::async_runtime::block_on(source.start_with(options, factory, host))?;
    if let Some(launcher) = launcher {
        let windows = SandboxWindows::new(sandbox_hooks(
            app,
            backend.problems(),
            backend.extension_hosts(),
        ));
        launcher.install(windows.clone());
        app.manage(windows);
    }
    open(app, options, backend)
}

/// What the workspaces' editor windows ask of the shell: links go to the default browser or are
/// reported as a problem, as the main window's do.
fn sandbox_hooks<R: Runtime>(
    app: &AppHandle<R>,
    problems: &Arc<Problems>,
    extension_hosts: EndExtensionHosts,
) -> Hooks {
    Hooks {
        open_external: opener(app, problems),
        refused: refuser(problems),
        new_window: window_reporter(problems),
        last_closed: ends_extension_hosts(extension_hosts),
    }
}

/// The hook for a workspace's last window closing: the guest ends the editor's extension hosts and
/// what they started, so a closed window leaves no extension host behind. The window system
/// calls it, so the guest is asked from the async runtime; a workspace that is stopping already
/// has nothing to end.
fn ends_extension_hosts(ends: EndExtensionHosts) -> Arc<dyn Fn(&WorkspaceName) + Send + Sync> {
    Arc::new(move |workspace| {
        let (ends, workspace) = (Arc::clone(&ends), workspace.clone());
        tauri::async_runtime::spawn(async move {
            match ends(workspace.clone()).await {
                Ok(()) => {
                    tracing::debug!(%workspace, "extension hosts ended after the last window closed");
                }
                Err(reason) => {
                    tracing::debug!(%workspace, %reason, "extension hosts not ended after the last window closed");
                }
            }
        });
    })
}

/// Opens the main window on a started backend, then keeps the backend for the exit hook.
///
/// # Errors
///
/// [`ShellError::Tauri`] if the window can't open.
pub fn open<R: Runtime>(
    app: &AppHandle<R>,
    options: &Options,
    backend: Backend,
) -> Result<(), ShellError> {
    let window = open_main(
        app,
        backend.url(),
        backend.token(),
        options.dev_url.as_ref(),
        options.debug_port,
        opener(app, backend.problems()),
        refuser(backend.problems()),
    )?;
    tracing::info!(url = %backend.url(), "main window open");
    // The fixture is for developing the window: it leaves no marker in the user's data folder.
    let marker_folder = if options.fixture {
        None
    } else {
        data_folder(options)
    };
    if let Some(probe) = &options.sandbox_probe {
        open_probe(app, probe, &data_folder(options).unwrap_or_default())?;
    }
    desktop::attach(app, &window, &backend, marker_folder);
    app.manage(backend);
    Ok(())
}

/// What the smoke test's window asks of the shell: its links are logged, so a test run starts no
/// browser.
fn probe_hooks() -> Hooks {
    Hooks {
        open_external: Arc::new(
            |url| tracing::info!(%url, "sandbox window: opened in the browser"),
        ),
        refused: Arc::new(|url| tracing::info!(%url, "sandbox window: link refused")),
        new_window: Arc::new(|workspace, end| {
            tracing::info!(%workspace, ?end, "sandbox window: new window");
        }),
        last_closed: Arc::new(|workspace| {
            tracing::info!(%workspace, "sandbox window: last window closed");
        }),
    }
}

/// Opens the smoke test's workspace window. Its links are logged instead of opened, so a test run
/// starts no browser.
fn open_probe<R: Runtime>(
    app: &AppHandle<R>,
    probe: &SandboxProbe,
    data: &std::path::Path,
) -> Result<(), ShellError> {
    let windows = SandboxWindows::new(probe_hooks());
    let mut origin = probe.url.clone();
    origin.set_path("/");
    origin.set_query(None);
    let target = Target {
        origin,
        data_folder: sandbox_window::data_folder(&data.join(WEBVIEW_FOLDER), &probe.workspace),
        workspace: probe.workspace.clone(),
        settings: WindowSettings {
            zoom_hotkeys: true,
            clipboard_read: ClipboardRead::Ask,
        },
        cookie: None,
        debug_port: Some(probe.debug_port),
    };
    let window = windows.open(app, &target)?;
    window.navigate(probe.url.clone())?;
    tracing::info!(url = %probe.url, "sandbox window open");
    app.manage(windows);
    Ok(())
}

/// The exit hook, the quit sequence: close every window but the main one (the workspaces' editor
/// windows), then stop the backend, which trims and stops every workspace before its API ends.
/// Says what the backend's shutdown did; `None` when there was no backend or it had stopped
/// already, so the hook is safe to run twice.
pub fn stop<R: Runtime>(app: &AppHandle<R>) -> Option<HostShutdown> {
    for (label, window) in app.webview_windows() {
        if label != MAIN_LABEL {
            // A window that is already going away can't be closed again; nothing is lost.
            let _ = window.destroy();
        }
    }
    let shutdown = tauri::async_runtime::block_on(app.try_state::<Backend>()?.shutdown())?;
    for warning in quit_warnings(&shutdown) {
        tracing::warn!("{warning}");
    }
    tracing::info!(steps = ?shutdown.steps, "stopped");
    Some(shutdown)
}

/// What the log says about a quit that did not go cleanly: each workspace whose disk was not
/// trimmed or whose machine did not stop, and the operations that were ended unfinished.
fn quit_warnings(shutdown: &HostShutdown) -> Vec<String> {
    let report = &shutdown.sandboxes;
    let mut lines: Vec<String> = report
        .untrimmed()
        .into_iter()
        .map(|(sandbox, why)| {
            format!("the disk of {sandbox} was not trimmed: {why}; Reclaim space trims it later")
        })
        .chain(
            report
                .unstopped()
                .into_iter()
                .map(|(sandbox, why)| format!("{sandbox} did not stop cleanly: {why}")),
        )
        .collect();
    if shutdown.operations_ended > 0 {
        lines.push(format!(
            "{} workspace operations were still running and were ended",
            shutdown.operations_ended
        ));
    }
    lines
}

/// Runs the app until it quits. The close button hides the window to the tray or quits, as Settings
/// say ([`desktop`]).
///
/// # Errors
///
/// [`ShellError`] if the app can't start.
pub fn run(options: &Options) -> Result<(), ShellError> {
    // The first subscriber wins; a second `run` in one process (tests) keeps the first.
    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .try_init();
    // Every panic after this writes a crash file into the data folder (nothing is sent).
    if let Some(folder) = data_folder(options) {
        crash::install(folder.join(crash::FOLDER));
    }
    // Before the runtime starts any machine: if the shell dies, Windows ends them with it.
    puddle_lifecycle::job::contain_this_process()?;
    // While this is the only thread: the host pins the process environment.
    let source = Source::prepare(options, &SystemPlatform::new().with_dev_override_from_env());
    let options = options.clone();
    // Single-instance first: a second launch hands over to the running app and ends here, before
    // the error of its unlocked data folder could show.
    let builder =
        tauri::Builder::default().plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            focus_main(app);
        }));
    let app = with_plugins(builder)
        .setup(move |app| {
            start(app.handle(), &options, source)?;
            if let Err(err) = desktop::install_quit_shortcut(app.handle()) {
                tracing::warn!(%err, "Ctrl+Q will not quit");
            }
            Ok(())
        })
        .build(context())?;
    app.run(|handle, event| {
        if matches!(event, RunEvent::Exit) {
            let _ = stop(handle);
        }
    });
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use puddle_lifecycle::{ShutdownReport, StopOutcome, TrimOutcome, WorkspaceOutcome};
    use puddle_types::SandboxName;
    use std::sync::Mutex;

    use super::*;

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
                    Err("the workspace is not running".to_owned())
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
        (hooks.last_closed)(&WorkspaceName::new("demo").unwrap());
        // A workspace that has stopped has nothing to end: not the user's concern either.
        (hooks.last_closed)(&WorkspaceName::new("stopped").unwrap());
        // Each ask runs in its own task, so the order is not the calls'.
        let mut seen = wait_for(&asked, 2);
        seen.sort();
        assert_eq!(seen, ["demo", "stopped"]);
        assert_eq!(problems.list().len(), 0, "{:?}", problems.list());
    }

    #[test]
    fn closing_a_popup_asks_nothing_and_the_last_window_asks_once() {
        let app = tauri::test::mock_app();
        let problems = Arc::new(Problems::new(Arc::new(puddle_api::EventHub::default())));
        let (ends, asked) = recording_ends();
        let windows = SandboxWindows::new(sandbox_hooks(app.handle(), &problems, ends));
        let data = tempfile::tempdir().unwrap();
        let demo = WorkspaceName::new("demo").unwrap();
        let target = Target {
            origin: "http://demo.localhost:4800/".parse().unwrap(),
            data_folder: sandbox_window::data_folder(data.path(), &demo),
            workspace: demo.clone(),
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
        let ended = windows.on_event(demo.clone());
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
            workspace: WorkspaceName::new("probe").unwrap(),
            url: Url::parse("http://probe.localhost:4900/attack.html").unwrap(),
            debug_port: 9444,
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
        let probe = SandboxProbe::parse(page, Some("9444")).unwrap();
        assert_eq!((probe.url.path(), probe.debug_port), ("/attack.html", 9444));
        assert_eq!(SandboxProbe::parse(None, Some("9444")), None);
        assert_eq!(SandboxProbe::parse(page, None), None);
        assert_eq!(SandboxProbe::parse(Some("not a url"), Some("9444")), None);
        assert_eq!(SandboxProbe::parse(page, Some("high")), None);
        // The environment is shared between tests: only a run with neither value set shows this.
        assert!(SandboxProbe::from_env().is_none());
    }

    #[test]
    fn the_probe_window_logs_its_links_and_asks_nothing_of_the_browser() {
        let hooks = probe_hooks();
        let url = Url::parse("https://example.org/x").unwrap();
        (hooks.open_external)(&url);
        (hooks.refused)(&url);
        (hooks.new_window)(&WorkspaceName::new("probe").unwrap(), Err("no room"));
        (hooks.last_closed)(&WorkspaceName::new("probe").unwrap());
    }

    #[test]
    fn a_failed_new_window_reaches_the_pages_problem_list_and_a_later_one_withdraws_it() {
        let problems = Arc::new(Problems::new(Arc::new(puddle_api::EventHub::default())));
        let demo = WorkspaceName::new("demo").unwrap();
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
}

#[cfg(all(test, feature = "fixture"))]
mod fixture_tests {
    #![expect(
        clippy::unwrap_used,
        reason = "helpers outside #[test] fns run only in tests; a panic is how a test fails"
    )]

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
        assert!(std::net::TcpStream::connect(("127.0.0.1", port)).is_err());
        // The hook can run again (a second Exit event, or no backend at all).
        assert_eq!(stop(app.handle()), None);
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
}
