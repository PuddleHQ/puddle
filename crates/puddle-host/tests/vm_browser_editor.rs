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
use puddle_api::{
    BrowserTarget, CONNECTION_TOKEN_COOKIE, DesktopTarget, EditorSignIn, LaunchError, Launcher,
    WorkspaceRecord,
};
use puddle_compute_msb::MsbRuntime;
use puddle_store::{Actor, Effect, NewRule, Pattern, Scope};
use puddle_types::WorkspaceName;
use puddle_vm_tests::{BrowserClient, BrowserCookie, BrowserSession, PageId, PageRequest};
use serde_json::{Value, json};
use support::{Api, ended, hermetic_options};
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

/// Waits until the editor server answers its health check.
const EDITOR_UP: &str = "for i in $(seq 1 100); do curl -sf -o /dev/null http://127.0.0.1:41337/healthz && exit 0; sleep 1; done; exit 1";

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

/// The guest's extension hosts and the editor's terminal host (its pty host, which the window's
/// first terminal starts), counted.
const COUNT: &str = r#"
hosts=$(ps -eo args= | grep -c -e '--type=extensionHos[t]' || true)
pty=$(ps -eo args= | grep -c -e '--type=ptyHos[t]' || true)
echo "hosts=$hosts pty=$pty health=$(curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:41337/healthz)"
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
/// host (the shell's last-window cleanup) and keeps the terminal host; opening it again within the
/// grace finds the editor with the kept session (no second sign-in). A stop closes the window
/// before the guest goes; the next start makes a new password, and the window opens signed in
/// again with a new sign-in on the multi-root file the attach wrote, not on the volume the last
/// window showed, with one root per checkout and the volume.
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

    // A terminal in the window: the editor starts its terminal host for it.
    shell
        .browser
        .lock()
        .await
        .press(shell.page(&name), "Escape")
        .await
        .expect("keys");
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
    let with_terminal = counts_until(&runtime, &sandbox, "the terminal host", |line| {
        count(line, "pty") >= 1
    })
    .await;

    // The window closes: the front counts none, and the cleanup ends the real extension host and
    // leaves the terminal host and the server.
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
    let rows = shell
        .wait(&name, EXPLORER_READY, EXPLORER_ROWS, WORKBENCH_BUDGET)
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
    let browser = Arc::try_unwrap(shell)
        .ok()
        .map(|shell| shell.browser.into_inner());
    if let Some(browser) = browser {
        browser.quit().await.expect("the browser closed");
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
  socket.onopen = () => { window.state = { build: ${JSON.stringify(build)}, token: !!cookie('vscode-tkn'), keyPath, key }; };
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
async fn serving(runtime: &MsbRuntime, sandbox: &WorkspaceName, build: &str) {
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
    let mut options = hermetic_options();
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
    let sandbox = WorkspaceName::new(&name).unwrap();
    // The stand-ins are the only builds: the user's own rule refuses Microsoft's update service and
    // download host, which System managed would allow, so the agent neither installs nor stages a
    // real build beside them (a rule the user made decides before a rule set's allow).
    for site in [
        "update.code.visualstudio.com",
        "vscode.download.prss.microsoft.com",
    ] {
        host.store()
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
    let builds = format!("/workspaces/{name}/.puddle/vscode-server/servers");
    // On the volume, so it is there after the restart below.
    let tools = format!("/workspaces/{name}/.puddle/stand-in");
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
        shell
            .browser
            .into_inner()
            .quit()
            .await
            .expect("the browser closed");
    }
}
