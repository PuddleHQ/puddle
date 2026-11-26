// SPDX-License-Identifier: AGPL-3.0-or-later
//! The browser editor end to end on a real microVM (K on Linux KVM): an attach in the browser
//! opens the workspace's editor in a real browser (headless Chromium beside the VM, as the desktop
//! shell's webview is beside it), signed in the way the shell signs a window in, through the
//! host's own front, to the editor server running in the guest.
//!
//! The desktop shell is played by [`ShellStandIn`], a [`Launcher`] that does what the shell's
//! launcher does with a webview: it reads the editor's password through the reader the host hands
//! it, signs in to the front with it (the password is in the login's body, never in the URL), puts
//! the session cookie in the browser and only then opens the editor's origin; a close of the
//! workspace's windows closes its pages. When a workspace's last page closes, the test asks the
//! guest to end the extension hosts, which is what the shell's last-window hook asks.
//!
//! Linux only: only `vm-linux` installs the browser.
#![cfg(target_os = "linux")]
#![expect(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "VM test: a failed step fails the test"
)]

use puddle_host_test_support::support;
use puddle_host_test_support::vm_support;

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::future::BoxFuture;
use puddle_compute_msb::MsbRuntime;
use puddle_services::{
    workspaces::BrowserTarget, workspaces::CONNECTION_TOKEN_COOKIE, workspaces::DesktopTarget,
    workspaces::EditorSignIn, workspaces::LaunchError, workspaces::Launcher,
    workspaces::WorkspaceRecord,
};
use puddle_store::{Actor, Effect, NewRule, Pattern, Scope};
use puddle_types::WorkspaceId;
use puddle_vm_tests::{BrowserClient, BrowserCookie, BrowserSession, PageId, PageRequest};
use serde_json::{Value, json};
use support::{Api, ended, named_id_options};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use vm_support::{VmHost, guest, guest_within, start_vm_host_config};

/// The cookie code-server's login sets.
const SESSION_COOKIE: &str = "code-server-session";

/// A limit on a hang, not a bar on speed: the first load of the workbench in a debug guest on a
/// shared runner fetches a few MB of scripts through the front and starts an extension host.
const WORKBENCH_BUDGET: Duration = Duration::from_secs(240);

/// Opening a page and reading its first state: the front answers at once.
const PAGE_BUDGET: Duration = Duration::from_secs(60);

/// The workbench is up when its explorer lists rows; the result is each row's label.
const EXPLORER_READY: &str =
    "document.querySelectorAll('.explorer-folders-view .monaco-list-row .label-name').length >= 3";
const EXPLORER_ROWS: &str = "[...document.querySelectorAll('.explorer-folders-view .monaco-list-row')]\
     .map(r => ({ name: r.querySelector('.label-name')?.textContent, level: r.getAttribute('aria-level') }))";

/// What the window shows when its explorer does not get ready: where it is, the explorer's rows,
/// the side bar's view and any dialog or notification in front of it. Each pane header says
/// whether its pane is expanded, and `loading` says whether the Folders view's progress bar is
/// running: together they tell a collapsed pane from an explorer still resolving.
const WINDOW_STATE: &str = r"(() => {
  const text = (e, n) => (e?.textContent || '').replace(/\s+/g, ' ').slice(0, n);
  const all = (selector, n) => [...document.querySelectorAll(selector)].map(e => text(e, n));
  return {
    href: location.href,
    rows: all('.explorer-folders-view .monaco-list-row', 80),
    sidebar: text(document.querySelector('.part.sidebar .title-label'), 80),
    panes: [...document.querySelectorAll('.part.sidebar .pane-header')]
      .map(e => ({ title: text(e, 80), expanded: e.getAttribute('aria-expanded') })),
    loading: document.querySelector('.explorer-folders-view .monaco-progress-container')
      ?.classList.contains('active') ?? null,
    welcome: all('.part.sidebar .welcome-view-content', 300),
    dialogs: all('.monaco-dialog-box', 400),
    notifications: all('.notifications-toasts .notification-list-item', 300),
    status: text(document.querySelector('.part.statusbar'), 400),
  };
})()";

/// What the window keeps in its browser storage and which of its loads failed, for a window whose
/// explorer does not get ready. A web workbench opens a remote multi-root file from the copy its
/// last window cached (`CachedConfigurations`), then reads the file itself; when that read fails
/// or comes back empty, the window keeps the file's identity with no folder and says "No Folder
/// Opened" without an error. The cached copy says which of the two reads gave the folders, and
/// the failed loads name the addresses the console only counts.
const WINDOW_STORAGE: &str = "(async () => {
  const failed = performance.getEntriesByType('resource')
    .filter(e => e.responseStatus >= 400)
    .map(e => `${e.responseStatus} ${e.name}`);
  const text = v => typeof v === 'string' ? v : new TextDecoder().decode(v);
  const cached = await new Promise(done => {
    const open = indexedDB.open('vscode-web-db');
    open.onerror = () => done(`(no database: ${open.error})`);
    open.onsuccess = () => {
      const db = open.result;
      const store = 'vscode-userdata-store';
      if (!db.objectStoreNames.contains(store)) { done(`(no ${store})`); return; }
      const all = db.transaction(store, 'readonly').objectStore(store);
      const keys = all.getAllKeys();
      keys.onerror = () => done(`(keys not read: ${keys.error})`);
      keys.onsuccess = () => {
        const wanted = keys.result.filter(k => String(k).includes('CachedConfigurations'));
        if (!wanted.length) { done({}); return; }
        const found = {};
        for (const key of wanted) {
          const get = all.get(key);
          get.onsuccess = get.onerror = () => {
            found[key] = get.result === undefined ? null : text(get.result).slice(0, 2000);
            if (Object.keys(found).length === wanted.length) done(found);
          };
        }
      };
    };
  });
  return { failed, cached };
})()";

/// The guest's side of a window whose explorer does not get ready: the editor server's command
/// line, the multi-root file with its times (a write at the window's start shows in them), the
/// volume, the boot hook's last report (an attach rewrites the file through it), any cached copy
/// on the server, and the end of the server's own logs.
const SERVER_STATE: &str = r#"
ps -eo pid,etimes,args | grep -e '[c]ode-server' | cut -c1-2000
data=$(ps -eo args= | grep -o -e '--user-data-dir [^ ]*' | head -n 1 | cut -d' ' -f2)
for f in /workspace/.puddle/editor/*.code-workspace; do [ -f "$f" ] && { echo "== $f"; cat "$f"; echo; stat -c 'inode %i, %s bytes, modified %y, changed %z' "$f"; }; done
echo "== /workspace/.puddle/editor"; ls -la --time-style=full-iso /workspace/.puddle/editor/
echo "== boot report"; tail -n 20 /run/puddle/boot.log
for f in $(find "$data" -path '*CachedConfigurations*' -type f 2>/dev/null); do echo "== $f"; head -c 2000 "$f"; echo; done
echo "== volume"; ls -a /workspace/
for f in $(find "$data/logs" -type f 2>/dev/null | sort | tail -n 12); do echo "== $f"; tail -n 30 "$f"; done
echo "== agent"; tail -n 30 /run/puddle/agent.log
true
"#;

/// Waits in `sandbox`'s newest page until its explorer lists rows and reads `result`. When it does
/// not, the failure carries what the window and the guest show and which `step` of the story
/// waited (the backtrace names only this function), so a timeout names its cause.
async fn explorer(
    shell: &ShellStandIn,
    runtime: &MsbRuntime,
    sandbox: &WorkspaceId,
    step: &str,
    result: &str,
) -> Value {
    let page = shell.page(sandbox.as_str()).await;
    let request = PageRequest::new("", EXPLORER_READY)
        .result(result)
        .budget(WORKBENCH_BUDGET);
    let outcome = shell.browser.lock().await.wait(page, &request).await;
    match outcome {
        Ok(outcome) => {
            shell
                .page_errors
                .lock()
                .unwrap()
                .extend(outcome.page_errors);
            outcome.result
        }
        Err(e) => {
            let state = PageRequest::new("", "true")
                .result(WINDOW_STATE)
                .budget(PAGE_BUDGET);
            let window = shell.browser.lock().await.wait(page, &state).await;
            let window = match window {
                Ok(outcome) => outcome.result.to_string(),
                Err(e) => format!("(not read: {e})"),
            };
            let storage = PageRequest::new("", "true")
                .result(WINDOW_STORAGE)
                .budget(PAGE_BUDGET);
            let storage = match shell.browser.lock().await.wait(page, &storage).await {
                Ok(outcome) => outcome.result.to_string(),
                Err(e) => format!("(not read: {e})"),
            };
            let logs = PageRequest::new("", "true")
                .result(WINDOW_LOGS)
                .budget(PAGE_BUDGET);
            let logs = match shell.browser.lock().await.wait(page, &logs).await {
                Ok(outcome) => outcome.result.to_string(),
                Err(e) => format!("(not read: {e})"),
            };
            tracing::error!(workspace = %sandbox, step, %logs, "the window's own logs");
            // Logged before the guest is asked: a guest that does not answer fails the test in
            // `guest_within`, and the window's state must not be lost with it.
            tracing::error!(workspace = %sandbox, step, error = %e, %window, %storage, "the explorer did not get ready");
            // One reload of the same window, after the dump: folders on the second load say the
            // window's first read of the multi-root file lost a race, none say the server's answer
            // was wrong; either way the next failure names which.
            let reload = PageRequest::new("", "true")
                .result("(setTimeout(() => location.reload(), 0), true)")
                .budget(PAGE_BUDGET);
            let _ = shell.browser.lock().await.wait(page, &reload).await;
            let again = PageRequest::new("", EXPLORER_READY)
                .result("true")
                .budget(PAGE_BUDGET);
            let reloaded = shell.browser.lock().await.wait(page, &again).await;
            tracing::error!(workspace = %sandbox, step, shows_folders_after_reload = reloaded.is_ok(), "the same window reloaded");
            let server = guest_within(runtime, sandbox, SERVER_STATE, PAGE_BUDGET).await;
            panic!("{sandbox}'s explorer did not get ready ({step}); the guest:\n{server}");
        }
    }
}

/// Waits until the editor server answers its health check.
const EDITOR_UP: &str = "for i in $(seq 1 100); do curl -sf -o /dev/null http://127.0.0.1:41337/healthz && exit 0; sleep 1; done; exit 1";

/// Where the page is and whether it shows code-server's login form.
const WHERE: &str = "({ path: location.pathname, search: decodeURIComponent(location.search), \
     login: !!document.querySelector('input[type=password]') })";

/// A page's first load, running on its own task: it fails with the browser's message.
type PageLoad = tokio::task::JoinHandle<Result<(), String>>;

/// A browser session, in the system colour scheme `scheme` when one is named.
async fn browser_in(scheme: Option<&str>) -> BrowserSession {
    let client = BrowserClient::from_env().expect("browser client environment");
    match scheme {
        Some(scheme) => client.session_in_color_scheme(scheme).await,
        None => client.session().await,
    }
    .expect("the browser started")
}

/// The desktop shell's part, with a real browser for its webview (module docs).
struct ShellStandIn {
    browser: Arc<tokio::sync::Mutex<BrowserSession>>,
    /// The pages open per workspace, oldest first.
    pages: Arc<Mutex<BTreeMap<String, Vec<PageId>>>>,
    /// The page loads still running per workspace, oldest first: an attach returns once a window
    /// exists, as the shell's does, and the page's first answer comes after.
    loading: Mutex<BTreeMap<String, Vec<PageLoad>>>,
    /// The session kept per workspace, with the password it was made for (the shell reuses it).
    sessions: Mutex<BTreeMap<String, (String, String)>>,
    /// How many times the shell signed in to the front.
    sign_ins: Mutex<usize>,
    /// The workspaces whose windows the host asked to close, in order.
    closed: Mutex<Vec<String>>,
    /// The origins it opened, in order.
    opened: Mutex<Vec<String>>,
    /// The uncaught errors every page reported, in order.
    page_errors: Mutex<Vec<String>>,
    /// Whether the windows opened from now on log at trace level (see [`PAGE_TRACE`]).
    trace_pages: std::sync::atomic::AtomicBool,
}

impl ShellStandIn {
    async fn new() -> Arc<Self> {
        Self::new_in(None).await
    }

    /// [`ShellStandIn::new`] with a browser whose system colour scheme is `scheme`.
    async fn new_in(scheme: Option<&str>) -> Arc<Self> {
        let browser = browser_in(scheme).await;
        Arc::new(Self {
            browser: Arc::new(tokio::sync::Mutex::new(browser)),
            pages: Arc::default(),
            loading: Mutex::default(),
            sessions: Mutex::default(),
            sign_ins: Mutex::default(),
            closed: Mutex::default(),
            opened: Mutex::default(),
            page_errors: Mutex::default(),
            trace_pages: std::sync::atomic::AtomicBool::default(),
        })
    }

    /// Waits until every page opened for `workspace` has had its first answer. A load that failed
    /// fails the test here, where the test first needs the page.
    async fn loaded(&self, workspace: &str) {
        let handles = self
            .loading
            .lock()
            .unwrap()
            .remove(workspace)
            .unwrap_or_default();
        for handle in handles {
            handle
                .await
                .expect("the page load task ran")
                .unwrap_or_else(|e| panic!("{workspace}'s page did not open: {e}"));
        }
    }

    /// Closes `workspace`'s pages and the browser, and starts a new one (an empty data folder)
    /// whose system colour scheme is `scheme`; the next window signs in again by cookie.
    async fn restart_browser_in(&self, workspace: &str, scheme: &str) {
        self.close_pages(workspace).await;
        let fresh = browser_in(Some(scheme)).await;
        let old = std::mem::replace(&mut *self.browser.lock().await, fresh);
        old.quit().await.expect("the browser closed");
    }

    /// Closes the browser. A page load still running keeps it open: then the process ends it.
    async fn quit(self) {
        if let Ok(browser) = Arc::try_unwrap(self.browser) {
            browser
                .into_inner()
                .quit()
                .await
                .expect("the browser closed");
        }
    }

    /// The newest page open for `workspace`, once it has loaded.
    async fn page(&self, workspace: &str) -> PageId {
        self.loaded(workspace).await;
        *self.pages.lock().unwrap()[workspace]
            .last()
            .expect("an open page")
    }

    /// Waits in `workspace`'s newest page until `ready`, and reads `result`.
    async fn wait(&self, workspace: &str, ready: &str, result: &str, budget: Duration) -> Value {
        let page = self.page(workspace).await;
        self.wait_in(page, ready, result, budget).await
    }

    /// [`ShellStandIn::wait`] in a given page; the page's uncaught errors are kept.
    async fn wait_in(&self, page: PageId, ready: &str, result: &str, budget: Duration) -> Value {
        let outcome = self
            .browser
            .lock()
            .await
            .wait(
                page,
                &PageRequest::new("", ready).result(result).budget(budget),
            )
            .await
            .unwrap_or_else(|e| {
                let ready: String = ready.chars().take(200).collect();
                panic!("the page, waiting for `{ready}`: {e}")
            });
        self.page_errors.lock().unwrap().extend(outcome.page_errors);
        outcome.result
    }

    /// The page open for `workspace` at `index`, oldest first, once it has loaded.
    async fn page_at(&self, workspace: &str, index: usize) -> PageId {
        self.loaded(workspace).await;
        self.pages.lock().unwrap()[workspace][index]
    }

    /// Presses `keys` in the newest page of `workspace`.
    async fn press(&self, workspace: &str, keys: &str) {
        let page = self.page(workspace).await;
        self.browser
            .lock()
            .await
            .press(page, keys)
            .await
            .expect("keys");
    }

    /// Types `text` into what has focus in the newest page of `workspace`.
    async fn type_text(&self, workspace: &str, text: &str) {
        let page = self.page(workspace).await;
        self.browser
            .lock()
            .await
            .type_text(page, text)
            .await
            .expect("typed");
    }

    /// Kills the newest page's renderer, as a killed browser would, and forgets the page.
    async fn crash_newest(&self, workspace: &str) {
        let page = self.page(workspace).await;
        self.browser
            .lock()
            .await
            .crash(page)
            .await
            .expect("the page crashed");
        self.pages
            .lock()
            .unwrap()
            .get_mut(workspace)
            .expect("pages")
            .retain(|open| *open != page);
    }

    /// Waits until some row of `selector` in `page` holds `needle` and returns every row's text.
    async fn rows_with(&self, page: PageId, selector: &str, needle: &str) -> Vec<String> {
        let selector = serde_json::to_string(selector).unwrap();
        let needle = serde_json::to_string(needle).unwrap();
        let rows = format!(
            "[...document.querySelectorAll({selector})].map(r => r.textContent.replace(/\\s+/g, ' ').trim())"
        );
        let ready = format!("{rows}.some(r => r.includes({needle}))");
        let found = self.wait_in(page, &ready, &rows, WORKBENCH_BUDGET).await;
        serde_json::from_value(found).unwrap()
    }

    /// Closes `workspace`'s pages, as the user closes its windows.
    async fn close_pages(&self, workspace: &str) {
        self.loaded(workspace).await;
        let pages = self
            .pages
            .lock()
            .unwrap()
            .remove(workspace)
            .unwrap_or_default();
        let mut browser = self.browser.lock().await;
        for page in pages {
            browser.close(page).await.expect("the page closed");
        }
    }

    /// Puts the cookie that signs the window in into the browser, as the shell does before the
    /// window navigates.
    async fn session(
        &self,
        workspace: &WorkspaceRecord,
        target: &BrowserTarget,
    ) -> Result<(), LaunchError> {
        let name = workspace.name.to_string();
        let domain = format!("{name}.localhost");
        let cookie = match &target.login {
            None => return Ok(()),
            Some(EditorSignIn::ConnectionToken(token)) => {
                BrowserCookie::host_only(CONNECTION_TOKEN_COOKIE, token.password().await?, domain)
                    .script_reads()
            }
            Some(EditorSignIn::Form(login)) => {
                let password = login.password().await?;
                let kept = self.sessions.lock().unwrap().get(&name).cloned();
                let session = match kept {
                    Some((for_password, session)) if for_password == password => session,
                    _ => {
                        let session = sign_in(target.front, &name, &password)
                            .await
                            .map_err(LaunchError::new)?;
                        *self.sign_ins.lock().unwrap() += 1;
                        self.sessions
                            .lock()
                            .unwrap()
                            .insert(name.clone(), (password, session.clone()));
                        session
                    }
                };
                BrowserCookie::host_only(SESSION_COOKIE, session, domain)
            }
        };
        self.browser
            .lock()
            .await
            .add_cookies(&[cookie])
            .await
            .map_err(|e| LaunchError::new(e.to_string()))
    }
}

impl Launcher for ShellStandIn {
    fn open_desktop<'a>(
        &'a self,
        _workspace: &'a WorkspaceRecord,
        _target: &'a DesktopTarget,
    ) -> BoxFuture<'a, Result<(), LaunchError>> {
        Box::pin(async { Err(LaunchError::new("no desktop editor in this test")) })
    }

    fn open_browser<'a>(
        &'a self,
        workspace: &'a WorkspaceRecord,
        target: &'a BrowserTarget,
    ) -> BoxFuture<'a, Result<(), LaunchError>> {
        Box::pin(async move {
            self.session(workspace, target).await?;
            self.opened.lock().unwrap().push(target.origin.clone());
            // The shell's launcher returns once the window is built and the page loads after; a
            // load waited for here would run into the API's request limit on a slow host.
            let mut address = start_address(target);
            if self.trace_pages.load(std::sync::atomic::Ordering::Relaxed) {
                address.push_str(PAGE_TRACE);
            }
            let request = PageRequest::new(address, "true")
                .on_commit()
                .budget(PAGE_BUDGET);
            let (browser, pages) = (Arc::clone(&self.browser), Arc::clone(&self.pages));
            let name = workspace.name.to_string();
            let load = tokio::spawn(async move {
                let (page, _) = browser
                    .lock()
                    .await
                    .open(&request)
                    .await
                    .map_err(|e| e.to_string())?;
                pages.lock().unwrap().entry(name).or_default().push(page);
                Ok(())
            });
            self.loading
                .lock()
                .unwrap()
                .entry(workspace.name.to_string())
                .or_default()
                .push(load);
            Ok(())
        })
    }

    fn close_browser<'a>(&'a self, workspace: &'a WorkspaceId) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            self.closed.lock().unwrap().push(workspace.to_string());
            self.close_pages(workspace.as_str()).await;
        })
    }
}

/// Appended to a window's address when its log level is raised: the web workbench reads
/// `logLevel` from the `payload` query (a JSON list of pairs), and keeps its logs, the file
/// service's reads of the multi-root file among them, in the page's `vscode-logs-store`.
const PAGE_TRACE: &str = "&payload=%5B%5B%22logLevel%22%2C%22trace%22%5D%5D";

/// The window's own logs from `vscode-logs-store`: every file's name and size, and the lines of
/// the biggest ones that name a workspace file read, a cancel, an error or the remote file
/// system, so a window that shows no folders says which read of the multi-root file failed.
const WINDOW_LOGS: &str = "(async () => {
  const text = v => typeof v === 'string' ? v : new TextDecoder().decode(v);
  return await new Promise(done => {
    const open = indexedDB.open('vscode-web-db');
    open.onerror = () => done(`(no database: ${open.error})`);
    open.onsuccess = () => {
      const db = open.result;
      const store = 'vscode-logs-store';
      if (!db.objectStoreNames.contains(store)) { done(`(no ${store})`); return; }
      const all = db.transaction(store, 'readonly').objectStore(store);
      const keys = all.getAllKeys();
      keys.onerror = () => done(`(keys not read: ${keys.error})`);
      keys.onsuccess = () => {
        const wanted = keys.result;
        if (!wanted.length) { done([]); return; }
        const found = [];
        const interesting = /code-workspace|readFile|atomic|cancel|error|remoteFilesystem|RemoteFileSystem|workspace/i;
        for (const key of wanted) {
          const get = all.get(key);
          get.onsuccess = get.onerror = () => {
            const body = get.result === undefined ? '' : text(get.result);
            const lines = body.split('\\n').filter(l => interesting.test(l)).slice(-60);
            found.push({ key: String(key), bytes: body.length, lines });
            if (found.length === wanted.length) done(found);
          };
        }
      };
    };
  });
})()";

/// The address the shell's window opens: the editor's root with the place the host named, as the
/// shell builds it (`/?workspace=<file>` or `/?folder=<path>`), or the root alone.
fn start_address(target: &BrowserTarget) -> String {
    match &target.open {
        Some(open) => {
            let key = if open.is_workspace_file {
                "workspace"
            } else {
                "folder"
            };
            format!("{}/?{key}={}", target.origin, open.path)
        }
        None => format!("{}/", target.origin),
    }
}

/// Signs in to the front for `workspace` the way the desktop shell does: a form POST of the
/// password to `/login` on the workspace's origin, sent to the front's own address. Returns the
/// session cookie's value.
async fn sign_in(front: SocketAddr, workspace: &str, password: &str) -> Result<String, String> {
    let body = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("password", password)
        .finish();
    let request = format!(
        "POST /login HTTP/1.1\r\nHost: {workspace}.localhost:{}\r\ncontent-type: application/x-www-form-urlencoded\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        front.port(),
        body.len()
    );
    let mut stream = tokio::net::TcpStream::connect(front)
        .await
        .map_err(|e| format!("the front could not be reached: {e}"))?;
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    let mut answer = Vec::new();
    tokio::time::timeout(Duration::from_secs(30), stream.read_to_end(&mut answer))
        .await
        .map_err(|_| "the login did not answer".to_owned())?
        .map_err(|e| e.to_string())?;
    let answer = String::from_utf8_lossy(&answer);
    let head = answer.split("\r\n\r\n").next().unwrap_or_default();
    head.lines()
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("set-cookie")
                .then_some(value.trim())
        })
        .filter_map(|cookie| cookie.split(';').next()?.split_once('='))
        .find(|(name, value)| *name == SESSION_COOKIE && !value.is_empty())
        .map(|(_, value)| value.to_owned())
        .ok_or_else(|| {
            format!(
                "no session in the login's answer: {}",
                head.lines().next().unwrap_or_default()
            )
        })
}

/// The guest's extension hosts and the editor's terminal host (its pty host, which the window's
/// first terminal starts), counted.
const COUNT: &str = r#"
hosts=$(ps -eo args= | grep -c -e '--type=extensionHos[t]' || true)
pty=$(ps -eo args= | grep -c -e '--type=ptyHos[t]' || true)
shells=$(ps -eo args= | grep -c -e 'shellIntegration-bas[h]' || true)
echo "hosts=$hosts pty=$pty shells=$shells health=$(curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:41337/healthz)"
"#;

/// Stops every extension host (`STOP`), keeping it in the process table.
const STOP_HOSTS: &str = r#"
ps -eo pid=,args= | grep -e '--type=extensionHos[t]' | while read -r pid _; do kill -STOP "$pid"; done
echo stopped
"#;

/// Parses `key=<n>` out of [`COUNT`]'s line.
fn count(line: &str, key: &str) -> usize {
    line.split_whitespace()
        .find_map(|pair| pair.strip_prefix(&format!("{key}=")))
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("no {key} in {line}"))
}

/// Polls the guest's counts until `done` holds; a limit on a hang only.
async fn counts_until(
    runtime: &MsbRuntime,
    sandbox: &WorkspaceId,
    what: &str,
    done: impl Fn(&str) -> bool,
) -> String {
    puddle_test_kit::eventually_async(what, Duration::from_secs(90), || async {
        let line = guest(runtime, sandbox, COUNT).await;
        if done(&line) {
            Ok(line)
        } else {
            Err(format!("the guest shows {line}"))
        }
    })
    .await
}

/// Waits until the front counts some window of `workspace` (`open`) or none; a limit on a hang
/// only. The front counts the editor's connections, and one workbench window holds more than one
/// (its management and its extension host connections), so "open" is "at least one".
async fn windows_become(windows: &puddle_front::Windows, workspace: &WorkspaceId, open: bool) {
    let mut seen = windows.watch(workspace);
    tokio::time::timeout(Duration::from_secs(60), seen.wait_for(|n| (*n > 0) == open))
        .await
        .unwrap_or_else(|_| {
            panic!(
                "the front counts {} connections of {workspace}; wanted a window open: {open}",
                windows.connected(workspace)
            )
        })
        .expect("the count is there while the host runs");
}

/// K tier, code-server. An attach in the browser opens the workspace's editor signed in (no
/// login page, and the password nowhere in an address), and with the server already running on the
/// volume root the window itself opens the multi-root file with one root per checkout and the
/// volume; the front counts the window. Closing the window ends the guest's real extension
/// host (the shell's last-window cleanup) and keeps the terminal host; opening it again within the
/// grace finds the editor with the kept session (no second sign-in). A stop closes the window
/// before the guest goes; the next start makes a new password, and the window opens signed in
/// again with a new sign-in on the same file.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(clippy::too_many_lines, reason = "one story, read top to bottom")]
async fn vm_browser_code_server_opens_signed_in_shows_the_checkouts_and_its_close_ends_the_extension_hosts()
 {
    let release = std::path::PathBuf::from(std::env::var_os("PUDDLE_CODE_SERVER_DIR").expect(
        "PUDDLE_CODE_SERVER_DIR: the unpacked code-server release (ci/fetch-code-server.sh)",
    ));
    let shell = ShellStandIn::new().await;
    let mut options = named_id_options();
    options.launcher = shell.clone();
    let VmHost {
        host,
        runtime,
        settings,
        ..
    } = start_vm_host_config("wcs", options, |config| {
        config.guest.code_server = Some(release);
    })
    .await;
    let name = format!("{}-wcs", settings.prefix.as_str());
    let sandbox = WorkspaceId::new(&name).unwrap();
    let api = Api::new(host.url(), host.token());
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
    let root = "/workspace".to_owned();
    guest(
        &runtime,
        &sandbox,
        &format!(
            "( {EDITOR_UP} ) && cd {root} && git init -q api && git init -q web && echo loose >notes.txt"
        ),
    )
    .await;
    let attach = || async {
        let reply = api
            .post(
                &format!("/api/workspaces/{name}/attach"),
                &json!({"mode": "browser"}).to_string(),
            )
            .await;
        assert_eq!(reply.status, 200, "{}", reply.body);
        // The time the window was asked for, to read the guest's logs against.
        tracing::info!(workspace = %name, "attach answered");
        let answer: Value = serde_json::from_str(&reply.body).unwrap();
        assert_eq!(answer["opened"], true, "{answer}");
        answer
    };
    let windows = host.editor_windows();

    // The window opens signed in, on the workspace's origin, with no password in its address.
    let answer = attach().await;
    let opened = shell.opened.lock().unwrap().clone();
    let origin = opened.last().expect("the shell opened a window").clone();
    assert!(
        origin.starts_with(&format!("http://{name}.localhost:")),
        "{origin}"
    );
    assert!(answer.get("url").is_none_or(Value::is_null), "{answer}");
    let rows = explorer(&shell, &runtime, &sandbox, "first window", EXPLORER_ROWS).await;
    let place = shell.wait(&name, "true", WHERE, PAGE_BUDGET).await;
    assert_eq!(place["login"], false, "{place}");
    let password = guest(&runtime, &sandbox, "cat /run/puddle/editor-password").await;
    for seen in [&origin, &place.to_string()] {
        assert!(!seen.contains(password.trim()), "the password is in {seen}");
    }
    // The server was running with the volume root before the attach found two checkouts: the
    // window itself asks for the multi-root file the attach wrote, so no restart is needed.
    let file = format!("{root}/.puddle/editor/{name}.code-workspace");
    assert_eq!(place["search"], format!("?workspace={file}"), "{place}");
    let roots: Vec<&str> = rows
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["level"] == "1")
        .filter_map(|row| row["name"].as_str())
        .collect();
    assert!(
        roots.contains(&"api") && roots.contains(&"web") && roots.len() == 3,
        "{rows}"
    );
    assert_eq!(*shell.sign_ins.lock().unwrap(), 1);
    windows_become(windows, &sandbox, true).await;
    let open = counts_until(&runtime, &sandbox, "the window's extension host", |line| {
        count(line, "hosts") >= 1
    })
    .await;

    // A terminal in the window: the editor starts its terminal host for it.
    let page = shell.page(&name).await;
    for keys in ["Escape", "Control+Backquote"] {
        shell
            .browser
            .lock()
            .await
            .press(page, keys)
            .await
            .expect("keys");
    }
    shell
        .wait(
            &name,
            "!!document.querySelector('.terminal-wrapper .xterm')",
            "true",
            PAGE_BUDGET,
        )
        .await;
    let with_terminal = counts_until(&runtime, &sandbox, "the terminal host", |line| {
        count(line, "pty") >= 1
    })
    .await;
    // No "Trust Folder" prompt stands in the way (`--disable-workspace-trust`): the terminal's
    // shell started although nobody clicked anything.
    let shell_counts = counts_until(&runtime, &sandbox, "the terminal's shell", |line| {
        count(line, "shells") >= 1
    })
    .await;
    assert!(shell_counts.contains("health=200"), "{shell_counts}");
    shell
        .wait(
            &name,
            "!document.querySelector('.workspace-trust-editor, .monaco-dialog-box')",
            "true",
            PAGE_BUDGET,
        )
        .await;

    // The window closes: the front counts none, and the cleanup ends the real extension host and
    // leaves the terminal host and the server.
    // The server ends a window's extension host itself when the window closes cleanly, and does it
    // in the same moments the cleanup looks for it, so a live host to end is not a given. Stopping
    // the host first (a stopped process ignores `TERM` until it is continued) makes the closed
    // window leave one behind, so the cleanup has one to end, and the `KILL` after its grace is
    // what ends it.
    guest(&runtime, &sandbox, STOP_HOSTS).await;
    shell.close_pages(&name).await;
    windows_become(windows, &sandbox, false).await;
    let reply = host
        .workspaces()
        .end_extension_hosts(&sandbox)
        .await
        .expect("the agent answered");
    assert!(
        matches!(
            reply,
            puddle_agent_proto::host_control::HostReply::Done { hosts, .. } if hosts >= 1
        ),
        "{reply:?} (before: {open})"
    );
    let closed = guest(&runtime, &sandbox, COUNT).await;
    assert_eq!(count(&closed, "hosts"), 0, "{closed}");
    assert_eq!(
        count(&closed, "pty"),
        count(&with_terminal, "pty"),
        "{closed}"
    );
    assert!(closed.contains("health=200"), "{closed}");

    // Opened again within the grace: the kept session, no second sign-in, a new extension host.
    attach().await;
    explorer(
        &shell,
        &runtime,
        &sandbox,
        "reopened within the grace",
        "true",
    )
    .await;
    assert_eq!(
        shell.wait(&name, "true", WHERE, PAGE_BUDGET).await["login"],
        false
    );
    assert_eq!(*shell.sign_ins.lock().unwrap(), 1);
    windows_become(windows, &sandbox, true).await;
    counts_until(
        &runtime,
        &sandbox,
        "the reopened window's extension host",
        |line| count(line, "hosts") >= 1,
    )
    .await;

    // A stop closes the window before the guest goes.
    let before = guest(&runtime, &sandbox, "cat /run/puddle/editor-password").await;
    let stop = api.post(&format!("/api/workspaces/{name}/stop"), "").await;
    assert_eq!(stop.status, 202, "{}", stop.body);
    let end = events
        .until(ended(name_static), Duration::from_secs(300))
        .await;
    assert_eq!(end["step"], "done", "{end}");
    assert_eq!(*shell.closed.lock().unwrap(), std::slice::from_ref(&name));
    assert!(!shell.pages.lock().unwrap().contains_key(&name));
    windows_become(windows, &sandbox, false).await;

    // The start is a new boot with a new password: the shell signs in again and the window opens
    // signed in. The server started with the multi-root file the attach wrote, so the window
    // opens it, not the volume the last window showed, with one root per checkout and the volume.
    let start = api.post(&format!("/api/workspaces/{name}/start"), "").await;
    assert_eq!(start.status, 202, "{}", start.body);
    let end = events
        .until(ended(name_static), Duration::from_secs(300))
        .await;
    assert_eq!(end["step"], "done", "{end}");
    guest(&runtime, &sandbox, EDITOR_UP).await;
    let after = guest(&runtime, &sandbox, "cat /run/puddle/editor-password").await;
    assert_ne!(after, before, "the new boot kept the old boot's password");
    let sign_ins = 2;
    attach().await;
    let rows = explorer(
        &shell,
        &runtime,
        &sandbox,
        "after the restart",
        EXPLORER_ROWS,
    )
    .await;
    let place = shell.wait(&name, "true", WHERE, PAGE_BUDGET).await;
    assert_eq!(place["login"], false, "{place}");
    assert_eq!(*shell.sign_ins.lock().unwrap(), sign_ins);
    let file = format!("{root}/.puddle/editor/{name}.code-workspace");
    assert_eq!(place["search"], format!("?workspace={file}"), "{place}");
    let roots: Vec<&str> = rows
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["level"] == "1")
        .filter_map(|row| row["name"].as_str())
        .collect();
    assert!(
        roots.contains(&"api") && roots.contains(&"web") && roots.len() == 3,
        "{rows}"
    );

    host.shutdown().await;
    drop(host);
    if let Some(shell) = Arc::into_inner(shell) {
        shell.quit().await;
    }
}

/// What a window of the editor shows of its colour theme: the workbench's theme class (`vs-dark`
/// for a dark theme, `vs` for a light one: VS Code's `ColorScheme`) and the scheme the browser
/// reports as the system's.
const WINDOW_THEME: &str = "(() => {
  const classes = [...(document.querySelector('.monaco-workbench')?.classList ?? [])];
  return {
    dark: classes.includes('vs-dark'),
    light: classes.includes('vs'),
    system: matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light',
  };
})()";

/// The first open of a window follows the system's colour scheme, in code-server itself: the
/// settings Puddle seeded make the real workbench pick its dark theme in a browser on a dark
/// system and its light theme in a browser on a light one (the control: the same server and the
/// same settings file, the other scheme). A user's own settings file stays what it was after the
/// server starts again.
///
/// What this cannot say is how the window looks on a real dark system: that stays a person's check.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vm_browser_code_server_first_open_follows_the_systems_colour_scheme_and_keeps_the_users_settings()
 {
    let release = std::path::PathBuf::from(std::env::var_os("PUDDLE_CODE_SERVER_DIR").expect(
        "PUDDLE_CODE_SERVER_DIR: the unpacked code-server release (ci/fetch-code-server.sh)",
    ));
    let shell = ShellStandIn::new_in(Some("dark")).await;
    let mut options = named_id_options();
    options.launcher = shell.clone();
    let VmHost {
        host,
        runtime,
        settings,
        ..
    } = start_vm_host_config("scheme", options, |config| {
        config.guest.code_server = Some(release);
    })
    .await;
    let name = format!("{}-scheme", settings.prefix.as_str());
    let sandbox = WorkspaceId::new(&name).unwrap();
    let api = Api::new(host.url(), host.token());
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

    // The file the agent seeded before the server's first start.
    let settings_file = "/workspace/.puddle/code-server/user-data/User/settings.json".to_owned();
    let seeded = guest(&runtime, &sandbox, &format!("cat {settings_file}")).await;
    assert!(
        seeded.contains(r#""window.autoDetectColorScheme": true"#),
        "{seeded}"
    );

    let attach = || async {
        let reply = api
            .post(
                &format!("/api/workspaces/{name}/attach"),
                &json!({"mode": "browser"}).to_string(),
            )
            .await;
        assert_eq!(reply.status, 200, "{}", reply.body);
    };
    let theme = |ws: &str| {
        let ws = ws.to_owned();
        let shell = Arc::clone(&shell);
        async move {
            shell
                .wait(
                    &ws,
                    "!!document.querySelector('.monaco-workbench')",
                    WINDOW_THEME,
                    WORKBENCH_BUDGET,
                )
                .await
        }
    };

    // A browser on a dark system: the window's workbench is in a dark theme.
    attach().await;
    let dark = theme(&name).await;
    assert_eq!(dark["system"], "dark", "{dark}");
    assert_eq!(dark["dark"], true, "the window did not open dark: {dark}");
    assert_eq!(dark["light"], false, "{dark}");

    // A browser on a light system, same server, same file: the window is in a light theme.
    shell.restart_browser_in(&name, "light").await;
    attach().await;
    let light = theme(&name).await;
    assert_eq!(light["system"], "light", "{light}");
    assert_eq!(
        light["light"], true,
        "the window did not open light: {light}"
    );
    assert_eq!(light["dark"], false, "{light}");

    // A user's own file is not touched by a start of the server.
    let own = r#"{"workbench.colorTheme": "Solarized Dark"}"#;
    guest(
        &runtime,
        &sandbox,
        &format!("printf '%s' '{own}' >{settings_file}"),
    )
    .await;
    guest(
        &runtime,
        &sandbox,
        "for p in /proc/[0-9]*; do tr '\\0' ' ' <$p/cmdline 2>/dev/null | \
         grep -q '[c]ode-server/lib/node' && kill -9 ${p#/proc/}; done; sleep 3; true",
    )
    .await;
    guest(&runtime, &sandbox, EDITOR_UP).await;
    let kept = guest(&runtime, &sandbox, &format!("cat {settings_file}")).await;
    assert_eq!(kept, own, "a start of the server changed the user's file");

    host.shutdown().await;
    drop(host);
    if let Some(shell) = Arc::into_inner(shell) {
        shell.quit().await;
    }
}

/// A stand-in for a build of Microsoft's VS Code server: what the host, the front and the agent
/// see of one, without a download from Microsoft (the VM tier has no internet). Started by the
/// agent as `bin/code-server` with the real server's arguments. Like the real server it refuses
/// every request but `/version` without the connection token (`?tkn=` or the cookie `vscode-tkn`:
/// microsoft/vscode `src/vs/server/node/serverConnectionToken.ts`); its page reads the token and
/// the secret-key path from its cookies as the workbench does (`web.main.ts`, `workbench.ts`),
/// mints the key at that path and keeps a socket open as a window does.
const STAND_IN_SERVER: &str = r#"// A stand-in for a build of Microsoft's VS Code server: what the host, the front and the agent
// see of one. Started as `bin/code-server` with the real server's arguments. Like the real server
// it refuses every request but `/version` without the connection token (`?tkn=` or the cookie
// `vscode-tkn`, microsoft/vscode `src/vs/server/node/serverConnectionToken.ts`); its page reads
// the token and the secret-key path from its cookies as the workbench does, mints the key and
// keeps a socket open as a window does.
const http = require("node:http");
const crypto = require("node:crypto");
const fs = require("node:fs");
const path = require("node:path");

const arg = (name) => process.argv[process.argv.indexOf(name) + 1];
const build = path.basename(__dirname);
const token = fs.readFileSync(arg("--connection-token-file"), "utf8").trim();
const cookie = (header, name) =>
  (header || "").split(/;\s*/).find((c) => c.startsWith(`${name}=`))?.slice(name.length + 1);
const allowed = (req) => {
  const url = new URL(req.url, "http://x");
  return url.searchParams.get("tkn") === token || cookie(req.headers.cookie, "vscode-tkn") === token;
};

const page = `<!doctype html><title>stand-in</title><script>
const cookie = (n) => document.cookie.split('; ').find((c) => c.startsWith(n + '='))?.slice(n.length + 1);
(async () => {
  const keyPath = cookie('vscode-secret-key-path');
  const answer = await fetch(keyPath, { method: 'POST' });
  const key = [...new Uint8Array(await answer.arrayBuffer())].map((b) => b.toString(16).padStart(2, '0')).join('');
  const socket = new WebSocket('ws://' + location.host + '/socket');
  socket.onopen = () => { window.state = { build: ${JSON.stringify(build)}, argv: ${JSON.stringify(process.argv.slice(2))}, token: !!cookie('vscode-tkn'), keyPath, key }; };
})();
</script>`;

const server = http.createServer((req, res) => {
  if (req.url === "/version") return res.end(build);
  if (!allowed(req)) {
    res.writeHead(403, { "content-type": "text/plain" });
    return res.end("Forbidden.");
  }
  res.writeHead(200, { "content-type": "text/html" });
  res.end(page);
});
server.on("upgrade", (req, socket) => {
  if (!allowed(req)) return socket.end("HTTP/1.1 403 Forbidden\r\n\r\n");
  const accept = crypto
    .createHash("sha1")
    .update(req.headers["sec-websocket-key"] + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11")
    .digest("base64");
  socket.write(`HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: ${accept}\r\n\r\n`);
  // A close frame (opcode 8) is answered with one and the socket ends, as a WebSocket server does.
  socket.on("data", (frame) => { if ((frame[0] & 0x0f) === 8) socket.end(Buffer.from([0x88, 0x00])); });
  socket.on("end", () => socket.destroy());
  socket.on("error", () => {});
});
server.listen(Number(arg("--port")), arg("--host"));"#;

/// Puts a complete stand-in build `$1` ([`STAND_IN_SERVER`], written to `stand-in.js` beside this
/// script first)
/// in the workspace's builds folder `$2` and names it with the pointer `$3` (`current` for the
/// build to start, `staged` for the one to switch to). A build no pointer names is removed by the
/// agent once a build has run, so each is put there when it is named.
const INSTALL_STAND_IN: &str = r#"
set -e
c=$1; builds=$2; pointer=$3
mkdir -p "$builds/$c/bin"
cp "$(dirname "$0")/stand-in.js" "$builds/$c/server.js"
printf '#!/bin/sh\nexec /puddle/code-server/lib/node "$(dirname "$0")/../server.js" "$@"\n' >"$builds/$c/bin/code-server"
chmod 755 "$builds/$c/bin/code-server"
touch "$builds/$c/.puddle-installed"
chmod -R a+rX "$builds"
printf %s "$c" >"$builds/$pointer"
"#;

/// Two builds' names (40 hex digits, like a commit).
const BUILD_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const BUILD_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

/// The stand-in page's state once it minted its key and its socket is open.
const STAND_IN_READY: &str = "!!window.state";
const STAND_IN_STATE: &str = "window.state";

/// Waits until the guest's editor server says it is `build`; a limit on a hang only.
async fn serving(runtime: &MsbRuntime, sandbox: &WorkspaceId, build: &str) {
    let script = format!(
        "for i in $(seq 1 120); do [ \"$(curl -s --max-time 2 http://127.0.0.1:41337/version)\" = {build} ] && exit 0; sleep 1; done; \
         echo \"not serving {build}:\"; curl -s --max-time 2 http://127.0.0.1:41337/version; tail -n 30 /run/puddle/agent.log; exit 1"
    );
    // Two minutes of polling, inside a longer limit, so a miss prints the agent's log.
    guest_within(runtime, sandbox, &script, Duration::from_secs(240)).await;
}

/// What the front answers for `path` on `workspace`'s origin to a client with no cookie.
async fn without_a_cookie(front: SocketAddr, workspace: &str, path: &str) -> String {
    let mut stream = tokio::net::TcpStream::connect(front).await.unwrap();
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {workspace}.localhost:{}\r\nconnection: close\r\n\r\n",
        front.port()
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut answer = Vec::new();
    tokio::time::timeout(Duration::from_secs(30), stream.read_to_end(&mut answer))
        .await
        .expect("the front answered")
        .unwrap();
    let answer = String::from_utf8_lossy(&answer).into_owned();
    let status = answer.lines().next().unwrap_or_default().to_owned();
    let body = answer
        .split("\r\n\r\n")
        .nth(1)
        .unwrap_or_default()
        .trim()
        .to_owned();
    format!("{status} {body}")
}

/// K tier, Microsoft's server (a stand-in build, [`STAND_IN_SERVER`]). The window opens with the
/// server's connection token in its cookie, which the server asks of every request (a client
/// without it gets 403), and gets the secret-storage key from the front. While the window is open
/// the front counts it and a staged build is not switched to; once it closes the host's ask
/// switches the server to it, and the window opened again runs on the new build with the same
/// key, so stored secrets survive the reopen and the update.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(clippy::too_many_lines, reason = "one story, read top to bottom")]
async fn vm_browser_microsoft_s_server_opens_with_its_token_and_switches_builds_only_with_no_window_open()
 {
    let release = std::path::PathBuf::from(std::env::var_os("PUDDLE_CODE_SERVER_DIR").expect(
        "PUDDLE_CODE_SERVER_DIR: the unpacked code-server release (ci/fetch-code-server.sh), whose node runs the stand-in",
    ));
    let shell = ShellStandIn::new().await;
    let mut options = named_id_options();
    options.launcher = shell.clone();
    let VmHost {
        host,
        runtime,
        settings,
        ..
    } = start_vm_host_config("wms", options, |config| {
        // Microsoft's server chosen, with the consent of the terms the popup shows today.
        let dir = config.paths.settings();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("global.json"),
            json!({
                "schema_version": 2,
                "vscode_server": {"server": "microsoft", "telemetry": false},
                "consents": {"vscode_server": {"state": "granted", "at": 1, "terms_version": puddle_settings::MICROSOFT_TERMS_VERSION}},
            })
            .to_string(),
        )
        .unwrap();
        config.guest.code_server = Some(release);
    })
    .await;
    let name = format!("{}-wms", settings.prefix.as_str());
    let sandbox = WorkspaceId::new(&name).unwrap();
    // The stand-ins are the only builds: the user's own rule refuses Microsoft's update service and
    // download host, which System managed would allow, so the agent neither installs nor stages a
    // real build beside them (a rule the user made decides before a rule set's allow).
    for site in [
        "update.code.visualstudio.com",
        "vscode.download.prss.microsoft.com",
    ] {
        puddle_store::testing::own(host.store(), &sandbox);
        host.store()
            .as_user(puddle_store::UserId::OWNER)
            .add_rule(&NewRule {
                scope: Scope::Workspace(sandbox.clone()),
                pattern: Pattern::parse(site).unwrap(),
                effect: Effect::Deny,
                expires_at: None,
                created_by: Actor::Cli,
            })
            .unwrap();
    }
    let api = Api::new(host.url(), host.token());
    let mut events = api.events().await;
    let name_static: &'static str = Box::leak(name.clone().into_boxed_str());
    let reply = api
        .post(
            "/api/workspaces",
            &json!({"name": name, "memory_mib": 2048}).to_string(),
        )
        .await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    let end = events
        .until(ended(name_static), Duration::from_secs(600))
        .await;
    assert_eq!(end["step"], "done", "{end}");

    // The builds go on the volume (the agent found none to start: no internet here), and the next
    // start runs the current one without asking Microsoft.
    let builds = "/workspace/.puddle/vscode-server/servers".to_owned();
    // On the volume, so it is there after the restart below.
    let tools = "/workspace/.puddle/stand-in".to_owned();
    guest(
        &runtime,
        &sandbox,
        &format!(
            "mkdir -p {tools} && cat >{tools}/stand-in.js <<'JS'\n{STAND_IN_SERVER}\nJS\n\
             cat >{tools}/install.sh <<'SH'\n{INSTALL_STAND_IN}\nSH\n\
             sh {tools}/install.sh {BUILD_A} {builds} current"
        ),
    )
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
    serving(&runtime, &sandbox, BUILD_A).await;

    // The window opens with the token: the page loads, reads the token and mints the key.
    let attach = || async {
        let reply = api
            .post(
                &format!("/api/workspaces/{name}/attach"),
                &json!({"mode": "browser"}).to_string(),
            )
            .await;
        assert_eq!(reply.status, 200, "{}", reply.body);
        let answer: Value = serde_json::from_str(&reply.body).unwrap();
        assert_eq!(answer["opened"], true, "{answer}");
    };
    attach().await;
    let first = shell
        .wait(&name, STAND_IN_READY, STAND_IN_STATE, PAGE_BUDGET)
        .await;
    assert_eq!(first["build"], BUILD_A, "{first}");
    assert_eq!(first["token"], true, "{first}");
    // The folder is trusted at the server: the real server's flag, so no prompt holds the shell.
    assert!(
        first["argv"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a == "--disable-workspace-trust"),
        "{first}"
    );
    assert_eq!(first["keyPath"], "/_puddle/mint-key", "{first}");
    let key = first["key"].as_str().unwrap().to_owned();
    assert_eq!(key.len(), 64, "{first}");
    assert_eq!(*shell.sign_ins.lock().unwrap(), 0, "no login form to post");
    let origin = shell.opened.lock().unwrap().last().unwrap().clone();
    let password = guest(&runtime, &sandbox, "cat /run/puddle/editor-password").await;
    assert!(
        !origin.contains(password.trim()),
        "the token is in {origin}"
    );
    // Without the token the server refuses, through the same front.
    let front: SocketAddr = format!("127.0.0.1:{}", origin.rsplit(':').next().unwrap())
        .parse()
        .unwrap();
    let refused = without_a_cookie(front, &name, "/").await;
    assert!(
        refused.starts_with("HTTP/1.1 403 ") && refused.contains("Forbidden."),
        "{refused}"
    );

    // A build is staged while the window is open: nothing switches.
    let windows = host.editor_windows();
    windows_become(windows, &sandbox, true).await;
    guest(
        &runtime,
        &sandbox,
        &format!("sh {tools}/install.sh {BUILD_B} {builds} staged"),
    )
    .await;
    assert_eq!(
        host.workspaces().switch_idle_editor_builds(windows).await,
        0
    );
    assert!(matches!(
        host.workspaces()
            .switch_editor_build(&sandbox, windows.connected(&sandbox))
            .await,
        Err(puddle_host::SwitchEditorBuildError::WindowsConnected(n)) if n >= 1
    ));
    serving(&runtime, &sandbox, BUILD_A).await;

    // The window closes: the host's ask switches the server to the staged build.
    shell.close_pages(&name).await;
    windows_become(windows, &sandbox, false).await;
    assert_eq!(
        host.workspaces().switch_idle_editor_builds(windows).await,
        1
    );
    serving(&runtime, &sandbox, BUILD_B).await;

    // Opened again: the new build, the same token cookie, the same key.
    attach().await;
    let again = shell
        .wait(&name, STAND_IN_READY, STAND_IN_STATE, PAGE_BUDGET)
        .await;
    assert_eq!(again["build"], BUILD_B, "{again}");
    assert_eq!(
        again["key"],
        key.as_str(),
        "the stored secrets' key changed: {again}"
    );
    // The new build accepted a connection, so it is confirmed and the old one goes.
    let kept = guest(
        &runtime,
        &sandbox,
        &format!("for i in $(seq 1 60); do [ -d {builds}/{BUILD_A} ] || break; sleep 1; done; cat {builds}/current; echo; ls {builds}"),
    )
    .await;
    assert!(
        kept.starts_with(BUILD_B) && !kept.contains(BUILD_A),
        "{kept}"
    );

    host.shutdown().await;
    drop(host);
    if let Some(shell) = Arc::into_inner(shell) {
        shell.quit().await;
    }
}

/// The workbench's terminal draws into the page (no text to read) unless its renderer is the DOM
/// one; the machine settings of the server say so, and the editor reads them when a window loads.
const READABLE_TERMINAL: &str = r#"
data=$(ps -eo args= | grep -o -e '--user-data-dir [^ ]*' | head -n 1 | cut -d' ' -f2)
mkdir -p "$data/Machine"
echo '{"terminal.integrated.gpuAcceleration":"off"}' >"$data/Machine/settings.json"
chown -R "$(stat -c %U "$data")" "$data/Machine"
"#;

/// A workspace with an editor server and a browser, ready for an attach.
struct Editor {
    vm: VmHost,
    shell: Arc<ShellStandIn>,
    api: Api,
    name: String,
    sandbox: WorkspaceId,
    /// The volume root in the guest.
    root: String,
}

impl Editor {
    /// Boots a workspace named for `tag` (no repositories) whose editor server runs with the
    /// reconnection grace `grace` (seconds, the settings' default when `None`), and makes the
    /// terminal readable.
    async fn start(tag: &str, grace: Option<u32>) -> Self {
        let release = std::path::PathBuf::from(std::env::var_os("PUDDLE_CODE_SERVER_DIR").expect(
            "PUDDLE_CODE_SERVER_DIR: the unpacked code-server release (ci/fetch-code-server.sh)",
        ));
        let shell = ShellStandIn::new().await;
        let mut options = named_id_options();
        options.launcher = shell.clone();
        let vm = start_vm_host_config(tag, options, |config| {
            config.guest.code_server = Some(release);
        })
        .await;
        let name = format!("{}-{tag}", vm.settings.prefix.as_str());
        let sandbox = WorkspaceId::new(&name).unwrap();
        let api = Api::new(vm.host.url(), vm.host.token());
        if let Some(grace) = grace {
            let body = json!({"workspace_defaults": {"reconnection_grace": grace}}).to_string();
            let reply = api.put("/api/settings", &body).await;
            assert_eq!(reply.status, 200, "{}", reply.body);
        }
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
        let editor = Self {
            root: "/workspace".to_owned(),
            vm,
            shell,
            api,
            name,
            sandbox,
        };
        editor.guest(EDITOR_UP).await;
        editor.guest(READABLE_TERMINAL).await;
        editor
    }

    async fn guest(&self, script: &str) -> String {
        guest(&self.vm.runtime, &self.sandbox, script).await
    }

    /// An attach in the browser, as the user's "Open isolated VS Code".
    async fn attach(&self) {
        let reply = self
            .api
            .post(
                &format!("/api/workspaces/{}/attach", self.name),
                &json!({"mode": "browser"}).to_string(),
            )
            .await;
        assert_eq!(reply.status, 200, "{}", reply.body);
    }

    /// Waits until the newest window's explorer is up.
    async fn explorer(&self, step: &str) {
        explorer(&self.shell, &self.vm.runtime, &self.sandbox, step, "true").await;
    }

    /// Opens the terminal panel in the newest window and waits until its shell shows a prompt,
    /// with the terminal focused.
    async fn open_terminal(&self) {
        let name = &self.name;
        self.shell.press(name, "Escape").await;
        self.shell.press(name, "Control+Backquote").await;
        self.shell
            .wait(
                name,
                "[...document.querySelectorAll('.xterm-rows > div')].some(r => r.textContent.trim() !== '')",
                "true",
                PAGE_BUDGET,
            )
            .await;
        self.shell
            .wait(
                name,
                "(document.querySelector('.xterm-helper-textarea').focus(), true)",
                "true",
                PAGE_BUDGET,
            )
            .await;
    }

    /// Runs `command` in the focused terminal.
    async fn terminal(&self, command: &str) {
        self.shell
            .type_text(&self.name, &format!("{command}\n"))
            .await;
    }

    /// The terminal's rows (long lines wrap) in the newest window once one holds `needle`.
    async fn terminal_shows(&self, needle: &str) -> Vec<String> {
        let page = self.shell.page(&self.name).await;
        self.shell
            .rows_with(page, ".xterm-rows > div", needle)
            .await
    }

    /// The extension hosts' process ids in the guest, as a sorted line.
    async fn extension_hosts(&self) -> Vec<u32> {
        let out = self
            .guest("ps -eo pid=,args= | grep -e '--type=extensionHos[t]' | while read -r pid _; do echo \"$pid\"; done")
            .await;
        out.split_whitespace()
            .filter_map(|p| p.parse().ok())
            .collect()
    }

    /// Whether process `pid` runs in the guest.
    async fn runs(&self, pid: u32) -> bool {
        self.guest(&format!(
            "kill -0 {pid} 2>/dev/null && echo up || echo down"
        ))
        .await
        .trim()
            == "up"
    }

    async fn finish(self) {
        self.vm.host.shutdown().await;
        drop(self.vm);
        if let Some(shell) = Arc::into_inner(self.shell) {
            shell.quit().await;
        }
    }
}

/// Files the editing test works on, in two checkouts and a loose file, owned by the editor's user.
const WORK_FILES: &str = r#"
set -e
cd "$1"
git init -q api && git init -q web && echo loose >notes.txt
mkdir -p api/lib/router api/.vscode
printf 'function route(path) { return path; }\nfunction other() { return 1; }\nmodule.exports = { route, other };\n' >api/lib/router/index.js
printf 'function web() { return 2; }\n' >web/other.js
printf 'const a = 1;\nconst b = a + 1;\nconsole.log("hit", b);\nsetTimeout(() => {}, 100000);\n' >api/app.js
printf '{"version":"0.2.0","configurations":[{"type":"node","request":"launch","name":"Run app","program":"${workspaceFolder}/app.js","runtimeExecutable":"/puddle/code-server/lib/node"}]}\n' >api/.vscode/launch.json
chown -R agent:agent .
"#;

/// K tier, code-server. Working in the window as the playbooks' editing steps do: Quick Open,
/// the command palette, a save that the guest sees, a search across both checkouts, a terminal
/// whose output is read back, a server started there that the Ports view lists on 3000, and a
/// Node breakpoint that is hit. A checkout added while the window is open shows in it, or the
/// window offers a reload. No page reports an uncaught error.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(clippy::too_many_lines, reason = "one story, read top to bottom")]
async fn vm_browser_code_server_edits_saves_searches_runs_a_terminal_lists_ports_and_hits_a_breakpoint()
 {
    let editor = Box::pin(Editor::start("wed", None)).await;
    let (shell, name, root) = (&editor.shell, &editor.name, &editor.root);
    editor
        .guest(&format!(
            "sh -c '{}' sh {root}",
            WORK_FILES.replace('\'', "'\\''")
        ))
        .await;
    editor.attach().await;
    editor.explorer("first window").await;
    let page = shell.page(name).await;

    // The command palette lists commands.
    shell.press(name, "F1").await;
    shell
        .wait(
            name,
            "!!document.querySelector('.quick-input-widget .quick-input-list')",
            "true",
            PAGE_BUDGET,
        )
        .await;
    shell.type_text(name, "Toggle Terminal").await;
    let palette = shell
        .rows_with(
            page,
            ".quick-input-list .monaco-list-row",
            "Toggle Terminal",
        )
        .await;
    assert!(
        palette
            .iter()
            .any(|row| row.starts_with("View: Toggle Terminal")),
        "{palette:?}"
    );
    shell.press(name, "Escape").await;

    // Quick Open finds a file by part of its name and shows where it is.
    shell.press(name, "Control+p").await;
    shell.type_text(name, "router").await;
    let files = shell
        .rows_with(page, ".quick-input-list .monaco-list-row", "lib/router")
        .await;
    assert!(
        files
            .iter()
            .any(|row| row.starts_with("index.js") && row.contains("api")),
        "{files:?}"
    );
    shell.press(name, "Enter").await;
    shell
        .wait(
            name,
            "document.querySelector('.tabs-container .tab.active')?.getAttribute('aria-label')?.startsWith('index.js') && document.querySelector('.monaco-editor .view-lines')?.textContent.replace(/\\u00a0/g, ' ').includes('function route')",
            "true",
            PAGE_BUDGET,
        )
        .await;

    // A save reaches the guest's file.
    shell.press(name, "Control+End").await;
    shell.type_text(name, "\n// saved in the window\n").await;
    shell.press(name, "Control+s").await;
    puddle_test_kit::eventually_async(
        "the save reaches the guest",
        Duration::from_secs(60),
        || async {
            let line = editor
                .guest(&format!(
                    "grep -c 'saved in the window' {root}/api/lib/router/index.js || true"
                ))
                .await;
            if line.trim() == "1" {
                Ok(())
            } else {
                Err(line)
            }
        },
    )
    .await;

    // A search across the checkouts lists the files of both.
    shell.press(name, "Control+Shift+f").await;
    shell
        .wait(
            name,
            "!!document.activeElement?.closest('.search-widget')",
            "true",
            PAGE_BUDGET,
        )
        .await;
    shell.type_text(name, "function").await;
    shell.press(name, "Enter").await;
    let found = shell
        .rows_with(page, ".search-view .monaco-list-row", "function web")
        .await;
    assert!(
        found
            .iter()
            .any(|row| row.starts_with("index.js") && row.contains("api")),
        "{found:?}"
    );
    assert!(
        found
            .iter()
            .any(|row| row.starts_with("other.js") && row.contains("web")),
        "{found:?}"
    );

    // A terminal command's output comes back (a computed value, not the typed text).
    editor.open_terminal().await;
    editor.terminal("echo out-$((6*7))-end").await;
    let screen = editor.terminal_shows("out-42-end").await;
    assert!(screen.iter().any(|row| row == "out-42-end"), "{screen:?}");

    // A server started in the terminal on 3000 is in the Ports view.
    editor
        .terminal("/puddle/code-server/lib/node -e \"require('http').createServer((q,r)=>r.end('hi')).listen(3000)\"")
        .await;
    puddle_test_kit::eventually_async(
        "the server answers in the guest",
        Duration::from_secs(60),
        || async {
            let code = editor
                .guest("curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:3000/ || true")
                .await;
            if code.trim() == "200" {
                Ok(())
            } else {
                Err(code)
            }
        },
    )
    .await;
    shell.press(name, "F1").await;
    shell
        .wait(
            name,
            "!!document.querySelector('.quick-input-widget .quick-input-list')",
            "true",
            PAGE_BUDGET,
        )
        .await;
    shell.type_text(name, "Ports: Focus on Ports View").await;
    shell
        .rows_with(
            page,
            ".quick-input-list .monaco-list-row",
            "Ports: Focus on Ports View",
        )
        .await;
    shell.press(name, "Enter").await;
    let ports = shell
        .rows_with(page, ".part.panel .monaco-list-row", "3000")
        .await;
    assert!(
        ports
            .iter()
            .any(|row| row.starts_with("3000") && row.contains("/proxy/3000/")),
        "{ports:?}"
    );

    // A breakpoint set on line 2 of a Node script is hit when it runs under the debugger.
    shell.press(name, "Control+p").await;
    shell.type_text(name, "app.js").await;
    shell
        .rows_with(page, ".quick-input-list .monaco-list-row", "app.js")
        .await;
    shell.press(name, "Enter").await;
    shell
        .wait(
            name,
            "document.querySelector('.tabs-container .tab.active')?.getAttribute('aria-label')?.startsWith('app.js') && document.querySelector('.monaco-editor .view-lines')?.textContent.replace(/\\u00a0/g, ' ').includes('setTimeout')",
            "true",
            PAGE_BUDGET,
        )
        .await;
    shell.press(name, "Control+g").await;
    shell.type_text(name, "2\n").await;
    shell.press(name, "F9").await;
    shell
        .wait(
            name,
            "document.querySelectorAll('.codicon-debug-breakpoint').length > 0",
            "true",
            PAGE_BUDGET,
        )
        .await;
    shell.press(name, "F5").await;
    let stack = shell
        .rows_with(
            page,
            ".debug-call-stack .monaco-list-row",
            "Paused on breakpoint",
        )
        .await;
    assert!(stack.iter().any(|row| row.contains("app.js")), "{stack:?}");
    shell.press(name, "Shift+F5").await;

    // A checkout added while the window is open: the first window shows it, or offers a reload.
    // The explorer is the side bar's view again before the attach opens a second window.
    shell.press(name, "Control+Shift+e").await;
    shell
        .wait(
            name,
            "document.querySelectorAll('.explorer-folders-view .monaco-list-row .label-name').length >= 3",
            "true",
            PAGE_BUDGET,
        )
        .await;
    editor
        .guest(&format!(
            "cd {root} && git init -q third && chown -R agent:agent third"
        ))
        .await;
    editor.attach().await;
    let first = shell.page_at(name, 0).await;
    let request = PageRequest::new(
        "",
        "[...document.querySelectorAll('.explorer-folders-view .monaco-list-row .label-name')].some(e => e.textContent === 'third') || [...document.querySelectorAll('.notifications-toasts .notification-list-item, .monaco-dialog-box')].some(e => /reload/i.test(e.textContent))",
    )
    .result(
        "({ third: [...document.querySelectorAll('.explorer-folders-view .monaco-list-row .label-name')].some(e => e.textContent === 'third'), reload: [...document.querySelectorAll('.notifications-toasts .notification-list-item, .monaco-dialog-box')].some(e => /reload/i.test(e.textContent)) })",
    )
    .budget(Duration::from_secs(120));
    let waited = shell.browser.lock().await.wait(first, &request).await;
    let outcome = match waited {
        Ok(outcome) => outcome.result,
        Err(e) => {
            let state = PageRequest::new("", "true")
                .result(WINDOW_STATE)
                .budget(PAGE_BUDGET);
            let window = shell.browser.lock().await.wait(first, &state).await;
            let file = editor
                .guest(&format!(
                    "cat {root}/.puddle/editor/{name}.code-workspace; ls -a {root}"
                ))
                .await;
            panic!(
                "the open window neither shows the added checkout nor offers a reload: {e}\nwindow: {window:?}\nguest:\n{file}"
            );
        }
    };
    eprintln!("a checkout added to the open window: {outcome}");

    let errors = shell.page_errors.lock().unwrap().clone();
    assert!(errors.is_empty(), "the pages reported errors: {errors:?}");
    editor.finish().await;
}

/// K tier, code-server, the reconnection grace set to 30 s. The browser dies abruptly (its renderer
/// is killed: no close frame, no handler runs), and the window opened again within the grace finds
/// the old window's extension host still running and the terminal back with its marker and the
/// same job (same pid). When the browser dies again and nobody comes back, the extension host is
/// freed about one grace later (not at once, not never), a `nohup` process lives, and so does the
/// terminal's job: the grace frees the extension host and ends orphaned debuggees (the attach spec), and
/// the server's terminal host and its jobs are outside that tree (the attach spec's process tree), so a terminal job
/// outliving the grace is current behaviour.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vm_browser_code_server_a_killed_browser_is_picked_up_within_the_grace_and_frees_its_extension_host_after_it()
 {
    // The seconds the server waits for a client that vanished; the settings' shortest value.
    const GRACE: u64 = 30;
    let editor = Box::pin(Editor::start("wgr", Some(u32::try_from(GRACE).unwrap()))).await;
    let (shell, name) = (&editor.shell, &editor.name);
    // Two checkouts and a loose file: the window shows three roots, which is when its explorer is up.
    editor
        .guest(&format!(
            "cd {} && git init -q api && git init -q web && echo loose >notes.txt && chown -R agent:agent .",
            editor.root
        ))
        .await;
    editor.attach().await;
    editor.explorer("first window").await;
    editor.open_terminal().await;
    editor.terminal("nohup sleep 91411 >/dev/null 2>&1 &").await;
    editor
        .terminal("echo marker-$((1900+82)); sleep 91412")
        .await;
    editor.terminal_shows("marker-1982").await;
    let job = editor
        .guest("pgrep -f '^sleep 91412$'")
        .await
        .trim()
        .parse::<u32>()
        .expect("the terminal's job runs once");
    let hosts = editor.extension_hosts().await;
    assert!(!hosts.is_empty(), "the window has an extension host");

    // Killed, and opened again within the grace: the window's host is still there, and the
    // terminal is back with the same job and what it printed.
    shell.crash_newest(name).await;
    editor.attach().await;
    for host in &hosts {
        assert!(
            editor.runs(*host).await,
            "extension host {host} ended before the grace did"
        );
    }
    editor
        .explorer("window opened again within the grace")
        .await;
    editor.open_terminal().await;
    editor.terminal_shows("marker-1982").await;
    let again = editor.guest("pgrep -f '^sleep 91412$'").await;
    assert_eq!(
        again.trim(),
        job.to_string(),
        "the terminal's job is another one"
    );

    // Killed again and nobody comes: the hosts of that window stay for the grace and go after it.
    // The window opened again runs a host of its own: the first window's host is not its host, and its grace
    // began at the first kill, so it ends about GRACE after that, whatever the second kill does.
    let first_window = hosts;
    let hosts: Vec<u32> = editor
        .extension_hosts()
        .await
        .into_iter()
        .filter(|host| !first_window.contains(host))
        .collect();
    assert!(
        !hosts.is_empty(),
        "the window opened again has an extension host"
    );
    shell.crash_newest(name).await;
    let killed = std::time::Instant::now();
    // flake-ok: an absence window: the host must still run a third of the grace after the kill, and
    // nothing signals that a process has not yet ended.
    tokio::time::sleep(Duration::from_secs(GRACE / 3)).await;
    for host in &hosts {
        assert!(
            editor.runs(*host).await,
            "extension host {host} ended long before the grace did"
        );
    }
    puddle_test_kit::eventually_async(
        "the grace frees the extension host",
        Duration::from_secs(90),
        || async {
            let mut left = Vec::new();
            for host in &hosts {
                if editor.runs(*host).await {
                    left.push(*host);
                }
            }
            if left.is_empty() {
                Ok(())
            } else {
                Err(format!("still running: {left:?}"))
            }
        },
    )
    .await;
    assert!(
        killed.elapsed() >= Duration::from_secs(GRACE / 2),
        "the extension host was freed after {:?}",
        killed.elapsed()
    );
    let nohup = editor.guest("pgrep -f '^sleep 91411$'").await;
    assert!(
        !nohup.trim().is_empty(),
        "the nohup process ended with the grace"
    );
    assert!(
        editor.runs(job).await,
        "the terminal's job ended with the grace"
    );

    editor.finish().await;
}

/// A TypeScript file whose second line has a symbol only the language server can describe: the
/// type of `total` is inferred from the call, so no text of the file says it.
const HOVER_FILES: &str = r#"
set -e
cd "$1"
git init -q api && git init -q web && echo loose >notes.txt
printf 'function add(a: number, b: number): number { return a + b; }\nconst total = add(1, 2);\n' >api/hover.ts
chown -R agent:agent .
"#;

/// The visible hover widgets' text (spaces normalised) and where the cursor is.
const HOVER_STATE: &str = "({ hover: [...document.querySelectorAll('.monaco-hover')].filter(e => !e.classList.contains('hidden') && e.offsetParent).map(e => e.textContent.replace(/\\u00a0/g, ' ')).join(' | '), cursor: /Ln \\d+, Col \\d+/.exec(document.querySelector('.part.statusbar')?.textContent ?? '')?.[0] })";

/// K tier, code-server. A TypeScript file opened in the window and a symbol hovered: the editor's
/// language server answers with the inferred type (the playbooks' "hover a symbol" step). The
/// answer is the server's own, so it is also what tells that the language server runs in the guest.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vm_browser_code_server_hovering_a_typescript_symbol_shows_the_type_the_language_server_infers()
 {
    let editor = Box::pin(Editor::start("whv", None)).await;
    let (shell, name, root) = (&editor.shell, &editor.name, &editor.root);
    editor
        .guest(&format!(
            "sh -c '{}' sh {root}",
            HOVER_FILES.replace('\'', "'\\''")
        ))
        .await;
    editor.attach().await;
    editor.explorer("window").await;
    let page = shell.page(name).await;

    shell.press(name, "Control+p").await;
    shell.type_text(name, "hover.ts").await;
    shell
        .rows_with(page, ".quick-input-list .monaco-list-row", "hover.ts")
        .await;
    shell.press(name, "Enter").await;
    shell
        .wait(
            name,
            "document.querySelector('.tabs-container .tab.active')?.getAttribute('aria-label')?.startsWith('hover.ts') && document.querySelector('.monaco-editor .view-lines')?.textContent.replace(/\\u00a0/g, ' ').includes('const total')",
            "true",
            PAGE_BUDGET,
        )
        .await;

    // The cursor goes into `total` on line 2 ("const " is six characters).
    shell.press(name, "Control+g").await;
    shell.type_text(name, "2\n").await;
    let at = |column: u32| {
        format!(
            "/Ln 2, Col {column}\\b/.test(document.querySelector('.part.statusbar')?.textContent ?? '')"
        )
    };
    shell.wait(name, &at(1), "true", PAGE_BUDGET).await;
    for _ in 0..7 {
        shell.press(name, "ArrowRight").await;
    }
    shell.wait(name, &at(8), "true", PAGE_BUDGET).await;

    // The language server starts with the window, so the first asks may find it still loading.
    // "Show or Focus Hover" asks again until the widget carries the type.
    let shown = puddle_test_kit::eventually_async(
        "the hover shows the inferred type of `total`",
        Duration::from_secs(90),
        || async {
            shell.press(name, "Control+k").await;
            shell.press(name, "Control+i").await;
            tokio::time::sleep(Duration::from_millis(1500)).await;
            let request = PageRequest::new("", "true")
                .result(HOVER_STATE)
                .budget(Duration::from_secs(5));
            let state = shell
                .browser
                .lock()
                .await
                .wait(page, &request)
                .await
                .map_err(|e| e.to_string())?
                .result;
            let hover = state["hover"].as_str().unwrap_or_default().to_owned();
            if hover.contains("const total: number") {
                Ok(hover)
            } else {
                Err(state.to_string())
            }
        },
    )
    .await;
    eprintln!("hover: {shown}");

    let errors = shell.page_errors.lock().unwrap().clone();
    assert!(errors.is_empty(), "the pages reported errors: {errors:?}");
    editor.finish().await;
}

/// Rounds before the memory baseline is read.
const WARM_UP_ROUNDS: u32 = 2;

/// What the guest's processes hold in memory, summed (bytes).
const GUEST_RSS: &str = "ps -eo rss= | awk '{ s += $1 } END { print s * 1024 }'";

/// K tier, code-server, soak bar. The window opened and closed ten times in a row: after each
/// close the guest has no extension host, and after the tenth the guest's memory is within 10 % of
/// what it was after the warm-up (the first rounds fill code-server's caches: the guest grew
/// 48 MB from round 1 to 2, then stayed within 4 % of round 3). `PUDDLE_VM_SOAK_BREAK=keep-extension-host`
/// stops the hosts at each close and skips the cleanup, which must turn the no-extension-host bar red.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vm_soak_browser_code_server_ten_opens_and_closes_leave_no_extension_host_and_the_memory_back()
 {
    let editor = Box::pin(Editor::start("wch", None)).await;
    let sandbox = &editor.sandbox;
    let windows = editor.vm.host.editor_windows();
    // Only the soak: a first load that misses the folders is a rare race, and its window's trace
    // logs are what name the read that failed (the other tests keep the default level).
    editor
        .shell
        .trace_pages
        .store(true, std::sync::atomic::Ordering::Relaxed);
    editor
        .guest(&format!(
            "cd {} && git init -q api && git init -q web && echo loose >notes.txt && chown -R agent:agent .",
            editor.root
        ))
        .await;
    let broken = std::env::var("PUDDLE_VM_SOAK_BREAK").is_ok_and(|v| v == "keep-extension-host");
    let mut first = 0;
    let mut last = 0;
    for round in 1..=10 {
        editor.attach().await;
        editor.explorer(&format!("window {round}")).await;
        windows_become(windows, sandbox, true).await;
        let open = counts_until(
            &editor.vm.runtime,
            sandbox,
            "the window's extension host",
            |l| count(l, "hosts") >= 1,
        )
        .await;
        if broken {
            editor.guest(STOP_HOSTS).await;
        }
        editor.shell.close_pages(&editor.name).await;
        windows_become(windows, sandbox, false).await;
        if !broken {
            editor
                .vm
                .host
                .workspaces()
                .end_extension_hosts(sandbox)
                .await
                .expect("the agent answered");
        }
        let closed = counts_until(
            &editor.vm.runtime,
            sandbox,
            "no extension host after the close",
            |l| count(l, "hosts") == 0,
        )
        .await;
        let rss: u64 = editor.guest(GUEST_RSS).await.trim().parse().unwrap();
        eprintln!("round {round}: guest memory {rss} bytes; open {open}; closed {closed}");
        if round == WARM_UP_ROUNDS + 1 {
            first = rss;
        }
        last = rss;
    }
    puddle_vm_tests::soak::judge_rss_back(first, last).unwrap();
    editor.finish().await;
}
