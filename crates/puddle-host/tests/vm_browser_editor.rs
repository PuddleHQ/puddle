// SPDX-License-Identifier: GPL-3.0-or-later
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

mod support;
mod vm_support;

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::future::BoxFuture;
use puddle_api::{BrowserTarget, DesktopTarget, LaunchError, Launcher, WorkspaceRecord};
use puddle_compute_msb::MsbRuntime;
use puddle_types::WorkspaceName;
use puddle_vm_tests::{BrowserClient, BrowserCookie, BrowserSession, PageId, PageRequest};
use serde_json::{Value, json};
use support::{Api, ended, hermetic_options};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use vm_support::{VmHost, guest, start_vm_host_config};

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

/// Where the page is and whether it shows code-server's login form.
const WHERE: &str = "({ path: location.pathname, search: decodeURIComponent(location.search), \
     login: !!document.querySelector('input[type=password]') })";

/// The desktop shell's part, with a real browser for its webview (module docs).
struct ShellStandIn {
    browser: tokio::sync::Mutex<BrowserSession>,
    /// The pages open per workspace, oldest first.
    pages: Mutex<BTreeMap<String, Vec<PageId>>>,
    /// The session kept per workspace, with the password it was made for (the shell reuses it).
    sessions: Mutex<BTreeMap<String, (String, String)>>,
    /// How many times the shell signed in to the front.
    sign_ins: Mutex<usize>,
    /// The workspaces whose windows the host asked to close, in order.
    closed: Mutex<Vec<String>>,
    /// The origins it opened, in order.
    opened: Mutex<Vec<String>>,
}

impl ShellStandIn {
    async fn new() -> Arc<Self> {
        let browser = BrowserClient::from_env()
            .expect("browser client environment")
            .session()
            .await
            .expect("the browser started");
        Arc::new(Self {
            browser: tokio::sync::Mutex::new(browser),
            pages: Mutex::default(),
            sessions: Mutex::default(),
            sign_ins: Mutex::default(),
            closed: Mutex::default(),
            opened: Mutex::default(),
        })
    }

    /// The newest page open for `workspace`.
    fn page(&self, workspace: &str) -> PageId {
        *self.pages.lock().unwrap()[workspace]
            .last()
            .expect("an open page")
    }

    /// Waits in `workspace`'s newest page until `ready`, and reads `result`.
    async fn wait(&self, workspace: &str, ready: &str, result: &str, budget: Duration) -> Value {
        let page = self.page(workspace);
        self.browser
            .lock()
            .await
            .wait(
                page,
                &PageRequest::new("", ready).result(result).budget(budget),
            )
            .await
            .unwrap_or_else(|e| panic!("{workspace}'s page: {e}"))
            .result
    }

    /// Closes `workspace`'s pages, as the user closes its windows.
    async fn close_pages(&self, workspace: &str) {
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

    async fn session(
        &self,
        workspace: &WorkspaceRecord,
        target: &BrowserTarget,
    ) -> Result<(), LaunchError> {
        let Some(login) = &target.login else {
            return Ok(());
        };
        let password = login.password().await?;
        let name = workspace.name.to_string();
        let kept = self.sessions.lock().unwrap().get(&name).cloned();
        let cookie = match kept {
            Some((for_password, cookie)) if for_password == password => cookie,
            _ => {
                let cookie = sign_in(target.front, &name, &password)
                    .await
                    .map_err(LaunchError::new)?;
                *self.sign_ins.lock().unwrap() += 1;
                self.sessions
                    .lock()
                    .unwrap()
                    .insert(name.clone(), (password, cookie.clone()));
                cookie
            }
        };
        self.browser
            .lock()
            .await
            .add_cookies(&[BrowserCookie::host_only(
                SESSION_COOKIE,
                cookie,
                format!("{name}.localhost"),
            )])
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
            let (page, _) = self
                .browser
                .lock()
                .await
                .open(
                    &PageRequest::new(format!("{}/", target.origin), "true")
                        .on_commit()
                        .budget(PAGE_BUDGET),
                )
                .await
                .map_err(|e| LaunchError::new(e.to_string()))?;
            self.opened.lock().unwrap().push(target.origin.clone());
            self.pages
                .lock()
                .unwrap()
                .entry(workspace.name.to_string())
                .or_default()
                .push(page);
            Ok(())
        })
    }

    fn close_browser<'a>(&'a self, workspace: &'a WorkspaceName) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            self.closed.lock().unwrap().push(workspace.to_string());
            self.close_pages(workspace.as_str()).await;
        })
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

/// The guest's extension hosts, and the shells of its terminals (children of the editor's pty
/// host), counted.
const COUNT: &str = r#"
hosts=$(ps -eo args= | grep -c -e '--type=extensionHos[t]' || true)
pty=$(for p in $(pgrep -f -- '--type=ptyHos[t]'); do pgrep -P "$p"; done | wc -l)
echo "hosts=$hosts terminals=$pty health=$(curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:41337/healthz)"
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
    sandbox: &WorkspaceName,
    what: &str,
    done: impl Fn(&str) -> bool,
) -> String {
    let mut line = String::new();
    for _ in 0..240 {
        line = guest(runtime, sandbox, COUNT).await;
        if done(&line) {
            return line;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    panic!("{what}: the guest still shows {line}");
}

/// Waits until the front counts some window of `workspace` (`open`) or none; a limit on a hang
/// only. The front counts the editor's connections, and one workbench window holds more than one
/// (its management and its extension host connections), so "open" is "at least one".
async fn windows_become(windows: &puddle_front::Windows, workspace: &WorkspaceName, open: bool) {
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
/// login page, and the password nowhere in an address), and the editor shows the volume's
/// checkouts; the front counts the window. Closing the window ends the guest's real extension
/// host (the shell's last-window cleanup) and keeps the terminal; opening it again within the
/// grace finds the editor with the kept session (no second sign-in). A stop closes the window
/// before the guest goes; after the next start the editor opens the multi-root file, one root per
/// checkout and the volume, with a new password and so a new sign-in.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(clippy::too_many_lines, reason = "one story, read top to bottom")]
async fn vm_browser_code_server_opens_signed_in_shows_the_checkouts_and_its_close_ends_the_extension_hosts()
 {
    let release = std::path::PathBuf::from(std::env::var_os("PUDDLE_CODE_SERVER_DIR").expect(
        "PUDDLE_CODE_SERVER_DIR: the unpacked code-server release (ci/fetch-code-server.sh)",
    ));
    let shell = ShellStandIn::new().await;
    let mut options = hermetic_options();
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
    let sandbox = WorkspaceName::new(&name).unwrap();
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
    let root = format!("/workspaces/{name}");
    guest(
        &runtime,
        &sandbox,
        &format!(
            "for i in $(seq 1 120); do curl -sf -o /dev/null http://127.0.0.1:41337/healthz && break; sleep 1; done; \
             cd {root} && git init -q api && git init -q web && echo loose >notes.txt"
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
    let rows = shell
        .wait(&name, EXPLORER_READY, EXPLORER_ROWS, WORKBENCH_BUDGET)
        .await;
    let place = shell.wait(&name, "true", WHERE, PAGE_BUDGET).await;
    assert_eq!(place["login"], false, "{place}");
    let password = guest(&runtime, &sandbox, "cat /run/puddle/editor-password").await;
    for seen in [&origin, &place.to_string()] {
        assert!(!seen.contains(password.trim()), "the password is in {seen}");
    }
    assert_eq!(place["search"], format!("?folder={root}"), "{place}");
    let names: Vec<&str> = rows
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|row| row["name"].as_str())
        .collect();
    for checkout in ["api", "web", "notes.txt"] {
        assert!(names.contains(&checkout), "{checkout} missing in {rows}");
    }
    assert_eq!(*shell.sign_ins.lock().unwrap(), 1);
    windows_become(windows, &sandbox, true).await;
    let open = counts_until(&runtime, &sandbox, "the window's extension host", |line| {
        count(line, "hosts") >= 1
    })
    .await;

    // A terminal in the window: its shell runs below the editor's pty host.
    shell
        .browser
        .lock()
        .await
        .press(shell.page(&name), "Control+Backquote")
        .await
        .expect("keys");
    shell
        .wait(
            &name,
            "!!document.querySelector('.terminal-wrapper .xterm')",
            "true",
            PAGE_BUDGET,
        )
        .await;
    let with_terminal = counts_until(&runtime, &sandbox, "the terminal's shell", |line| {
        count(line, "terminals") >= 1
    })
    .await;

    // The window closes: the front counts none, and the cleanup ends the real extension host and
    // leaves the terminal and the server.
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
        count(&closed, "terminals"),
        count(&with_terminal, "terminals"),
        "{closed}"
    );
    assert!(closed.contains("health=200"), "{closed}");

    // Opened again within the grace: the kept session, no second sign-in, a new extension host.
    attach().await;
    shell
        .wait(&name, EXPLORER_READY, "true", WORKBENCH_BUDGET)
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
    let stop = api.post(&format!("/api/workspaces/{name}/stop"), "").await;
    assert_eq!(stop.status, 202, "{}", stop.body);
    let end = events
        .until(ended(name_static), Duration::from_secs(300))
        .await;
    assert_eq!(end["step"], "done", "{end}");
    assert_eq!(*shell.closed.lock().unwrap(), std::slice::from_ref(&name));
    assert!(!shell.pages.lock().unwrap().contains_key(&name));
    windows_become(windows, &sandbox, false).await;

    // The next start opens the multi-root file the attach wrote: a root per checkout and the
    // volume. The password is new, so the shell signs in again.
    let start = api.post(&format!("/api/workspaces/{name}/start"), "").await;
    assert_eq!(start.status, 202, "{}", start.body);
    let end = events
        .until(ended(name_static), Duration::from_secs(300))
        .await;
    assert_eq!(end["step"], "done", "{end}");
    guest(
        &runtime,
        &sandbox,
        "for i in $(seq 1 120); do curl -sf -o /dev/null http://127.0.0.1:41337/healthz && break; sleep 1; done",
    )
    .await;
    attach().await;
    let rows = shell
        .wait(&name, EXPLORER_READY, EXPLORER_ROWS, WORKBENCH_BUDGET)
        .await;
    let place = shell.wait(&name, "true", WHERE, PAGE_BUDGET).await;
    assert_eq!(
        place["search"],
        format!("?workspace={root}/.puddle/editor/{name}.code-workspace"),
        "{place}"
    );
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
    assert_eq!(*shell.sign_ins.lock().unwrap(), 2);

    host.shutdown().await;
    drop(host);
    let browser = Arc::try_unwrap(shell)
        .ok()
        .map(|shell| shell.browser.into_inner());
    if let Some(browser) = browser {
        browser.quit().await.expect("the browser closed");
    }
}
