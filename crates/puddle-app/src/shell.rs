// SPDX-License-Identifier: AGPL-3.0-or-later
//! Wiring: plugins, the backend and the main window, and the exit hook (the quit sequence).

use std::path::PathBuf;
use std::sync::Arc;

use puddle_host::{HostOptions, HostShutdown, MsbFactory, RuntimeFactory, SystemPlatform};
use puddle_lifecycle::LifecycleError;
use puddle_log::{ScopedFilter, init_stderr};
use puddle_types::Problems;
use tauri::{AppHandle, Manager, RunEvent, Runtime};
use tauri_plugin_opener::OpenerExt;
use tracing_subscriber::filter::{LevelFilter, Targets};
use url::Url;

use crate::autostart::{self, LoginEntry};
use crate::backend::{Backend, BackendError, EndExtensionHosts, Source};
use crate::launcher::AppLauncher;
use puddle_settings::ClipboardRead;
use puddle_types::WorkspaceId;

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
    /// The workspace the window stands for (`probe` unless `PUDDLE_APP_SANDBOX_PROBE_WORKSPACE`
    /// names another id).
    pub workspace: WorkspaceId,
    /// The page to open, on a `<workspace>.localhost:<port>` origin.
    pub url: Url,
    /// WebView2's DevTools Protocol port for that window (Windows).
    pub debug_port: u16,
    /// Remove the workspace's webview data before the window opens, the way deleting the workspace
    /// does (`PUDDLE_APP_SANDBOX_PROBE_FORGET=1`), so a test can show what a real webview keeps
    /// after a delete.
    pub forget: bool,
}

impl SandboxProbe {
    /// The probe the environment asks for: `PUDDLE_APP_SANDBOX_PROBE` is the page,
    /// `PUDDLE_APP_SANDBOX_DEBUG_PORT` its debug port, and the optional
    /// `PUDDLE_APP_SANDBOX_PROBE_WORKSPACE` and `PUDDLE_APP_SANDBOX_PROBE_FORGET` the workspace id
    /// and the data removal.
    fn from_env() -> Option<Self> {
        let var = |name| std::env::var(name).ok();
        Self::parse(
            var("PUDDLE_APP_SANDBOX_PROBE").as_deref(),
            var("PUDDLE_APP_SANDBOX_DEBUG_PORT").as_deref(),
            var("PUDDLE_APP_SANDBOX_PROBE_WORKSPACE").as_deref(),
            var("PUDDLE_APP_SANDBOX_PROBE_FORGET").as_deref(),
        )
    }

    /// The probe for these values; none unless the page and the debug port are there and valid.
    /// A workspace id that is not one makes no probe either, so a typo cannot open `probe`.
    fn parse(
        url: Option<&str>,
        debug_port: Option<&str>,
        workspace: Option<&str>,
        forget: Option<&str>,
    ) -> Option<Self> {
        Some(Self {
            workspace: WorkspaceId::new(workspace.unwrap_or("probe")).ok()?,
            url: Url::parse(url?).ok()?,
            debug_port: debug_port?.parse().ok()?,
            forget: forget == Some("1"),
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
    /// The program was started by the sign-in entry ([`autostart::AT_LOGIN_FLAG`]): the window
    /// opens hidden in the tray, or minimized where there is no tray.
    pub at_login: bool,
    /// The system's start-at-sign-in entry the Settings switch is applied to. `None` in a run that
    /// must not touch it (tests, the fixture, a run over a data folder of its own).
    pub login: Option<Arc<dyn LoginEntry>>,
}

impl Options {
    /// The options the environment asks for (debug builds only).
    #[must_use]
    pub fn from_env() -> Self {
        let at_login = autostart::started_at_login(std::env::args_os());
        let options = if cfg!(debug_assertions) {
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
                ..Self::default()
            }
        } else {
            Self::default()
        };
        Self {
            at_login,
            login: manages_login(cfg!(debug_assertions)).then(autostart::system),
            ..options
        }
    }
}

/// Whether this build manages the user's sign-in entry: only an installed (release) build does.
/// A development build, a test run or the fixture would point the entry at itself, or remove the
/// entry of the installed program.
fn manages_login(debug_build: bool) -> bool {
    !debug_build
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
    let builder = builder
        .plugin(
            tauri_plugin_window_state::Builder::default()
                .with_filter(sandbox_window::remembers_place)
                .build(),
        )
        .plugin(tauri_plugin_notification::init());
    // The dialog plugin links `rfd` against comctl32 v6, which an executable only gets from a
    // manifest. The library's own unit-test executable has none and cargo gives it no link
    // argument key (build.rs covers integration tests), so on Windows it would not load
    // (0xc0000139). Leaving the plugin out of that build keeps `rfd` out of its link; the
    // integration tests (tests/acl.rs) and the program have the manifest and load it.
    #[cfg(not(test))]
    let builder = builder.plugin(tauri_plugin_dialog::init());
    builder.plugin(tauri_plugin_opener::init())
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
        move |workspace: &WorkspaceId, end: Result<(), &str>| match end {
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
    let mut host = HostOptions::system();
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
fn ends_extension_hosts(ends: EndExtensionHosts) -> Arc<dyn Fn(&WorkspaceId) + Send + Sync> {
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
    let tray = desktop::attach(app, &window, &backend, marker_folder, options.login.clone());
    if options.at_login {
        desktop::start_out_of_the_way(&window, tray);
    }
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
    if probe.forget {
        match sandbox_window::delete_data_folder(&data.join(WEBVIEW_FOLDER), &probe.workspace) {
            Ok(()) => tracing::info!(workspace = %probe.workspace, "sandbox window: data removed"),
            Err(error) => {
                tracing::warn!(workspace = %probe.workspace, %error, "sandbox window: data not removed");
            }
        }
    }
    let mut origin = probe.url.clone();
    origin.set_path("/");
    origin.set_query(None);
    let target = Target {
        origin,
        data_folder: sandbox_window::data_folder(&data.join(WEBVIEW_FOLDER), &probe.workspace),
        workspace: probe.workspace.clone(),
        display_name: probe.workspace.to_string(),
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

/// How long the quit waits for the guests to end their extension hosts: the agent answers within
/// about 4 s, and a guest that does not must not hold up the stop.
const QUIT_CLEANUP_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// The exit hook, the quit sequence: close every window but the main one (the workspaces'
/// editor windows), have the guest of each workspace that had one end its extension hosts, then
/// stop the backend, which trims and stops every workspace before its API ends.
/// Says what the backend's shutdown did; `None` when there was no backend or it had stopped
/// already, so the hook is safe to run twice.
pub fn stop<R: Runtime>(app: &AppHandle<R>) -> Option<HostShutdown> {
    // Before the windows go: their ends must not each ask the guest, the quit asks once below.
    let with_editor = app
        .try_state::<SandboxWindows>()
        .map(|windows| windows.begin_quit())
        .unwrap_or_default();
    for (label, window) in app.webview_windows() {
        if label != MAIN_LABEL {
            // A window that is already going away can't be closed again; nothing is lost.
            let _ = window.destroy();
        }
    }
    let backend = app.try_state::<Backend>()?;
    tauri::async_runtime::block_on(end_extension_hosts(
        &backend.extension_hosts(),
        with_editor,
        QUIT_CLEANUP_WAIT,
    ));
    let shutdown = tauri::async_runtime::block_on(backend.shutdown())?;
    for warning in quit_warnings(&shutdown) {
        tracing::warn!("{warning}");
    }
    tracing::info!(steps = ?shutdown.steps, "stopped");
    Some(shutdown)
}

/// Asks each of `workspaces`' guests, all at once, to end the editor's extension hosts, and waits
/// for the answers up to `wait`. A guest that cannot answer (the workspace already stopped, a
/// connection gone) is logged and the quit goes on: nothing may fail because of the sandbox.
async fn end_extension_hosts(
    ends: &EndExtensionHosts,
    workspaces: Vec<WorkspaceId>,
    wait: std::time::Duration,
) {
    let asks = workspaces.into_iter().map(|workspace| async move {
        match ends(workspace.clone()).await {
            Ok(()) => tracing::debug!(%workspace, "extension hosts ended before the quit"),
            Err(reason) => {
                tracing::warn!(%workspace, %reason, "extension hosts not ended before the quit");
            }
        }
    });
    if tokio::time::timeout(wait, futures_util::future::join_all(asks))
        .await
        .is_err()
    {
        tracing::warn!("some extension hosts were not ended before the quit: no answer in time");
    }
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

/// What a second start of the program does in the running one: bring the window forward, unless
/// the sign-in entry made that start (a copy is already running), which shows nothing.
fn on_second_start<R: Runtime>(app: &AppHandle<R>, args: &[String]) {
    bring_forward_unless_sign_in(args, || focus_main(app));
}

/// Calls `bring_forward` unless `args` carry the sign-in entry's flag. A seam of its own because
/// the mock window system keeps no focus state a test could read back.
fn bring_forward_unless_sign_in(args: &[String], bring_forward: impl FnOnce()) {
    if !autostart::started_at_login(args) {
        bring_forward();
    }
}

/// The variable that sets the log level of Puddle's crates (`puddle_log` has the syntax).
const PUDDLE_LOG_VAR: &str = "PUDDLE_LOG";

/// The standard filter variable, read when `PUDDLE_LOG` is not set: `debug`, or
/// `warn,puddle_host=trace` for one crate. It applies to every crate alike.
const RUST_LOG_VAR: &str = "RUST_LOG";

/// The filter `RUST_LOG` asks for, `info` when it is unset. A value that does not parse gives
/// `info` and comes back as the second item, so `run` can say so once logging is on.
fn log_targets(value: Option<&str>) -> (Targets, Option<String>) {
    let info = || Targets::new().with_default(LevelFilter::INFO);
    match value {
        None => (info(), None),
        Some(text) => text
            .parse()
            .map_or_else(|_| (info(), Some(text.to_owned())), |t| (t, None)),
    }
}

/// The filter the two variables ask for: `PUDDLE_LOG` when set, else `RUST_LOG`, else `info`. A
/// value that is not a filter gives `info` and a sentence for `run` to log once logging is on.
fn log_filter(puddle_log: Option<&str>, rust_log: Option<&str>) -> (ScopedFilter, Option<String>) {
    if let Some(text) = puddle_log {
        return ScopedFilter::parse(text).map_or_else(
            |_| {
                let said =
                    format!("{PUDDLE_LOG_VAR}={text:?} is not a log filter; logging at info");
                (ScopedFilter::default(), Some(said))
            },
            |filter| (filter, None),
        );
    }
    let (targets, bad) = log_targets(rust_log);
    let said = bad.map(|v| format!("{RUST_LOG_VAR}={v:?} is not a log filter; logging at info"));
    (ScopedFilter::uniform(targets), said)
}

/// Starts logging to stderr as the variables (`puddle_log`, `rust_log`) ask. The first subscriber
/// wins; a second call in one process (tests) keeps the first.
fn init_logging(puddle_log: Option<&str>, rust_log: Option<&str>) {
    let (filter, bad_filter) = log_filter(puddle_log, rust_log);
    init_stderr(filter, false);
    if let Some(said) = bad_filter {
        tracing::warn!("{said}");
    }
}

/// [`init_logging`] with the two variables as the process has them.
fn init_logging_from_env() {
    init_logging(
        std::env::var(PUDDLE_LOG_VAR).ok().as_deref(),
        std::env::var(RUST_LOG_VAR).ok().as_deref(),
    );
}

/// Runs the app until it quits. The close button hides the window to the tray or quits, as Settings
/// say ([`desktop`]).
///
/// # Errors
///
/// [`ShellError`] if the app can't start.
pub fn run(options: &Options) -> Result<(), ShellError> {
    init_logging_from_env();
    // Before the host reads a credential; `PUDDLE_ALLOW_DUMPS=1` keeps native dumps for diagnosis.
    puddle_no_dump::harden_from_env().log();
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
        tauri::Builder::default().plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            on_second_start(app, &args);
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
pub(crate) mod tests;

#[cfg(all(test, feature = "fixture"))]
mod fixture_tests;
