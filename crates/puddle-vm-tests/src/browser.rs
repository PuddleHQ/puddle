// SPDX-License-Identifier: GPL-3.0-or-later
//! A browser client for VM tests: node and headless Chromium, run beside the VM.
//!
//! The browser runs on the test host, the way a user's browser does: the browser editor's front
//! listens on the host's loopback (`<workspace>.localhost:<port>`) and Chromium reaches it there.
//! It does not run in a guest.
//!
//! - [`Node`] runs a node process with a time budget and returns what it printed.
//! - [`BrowserClient`] opens a page in Chromium, waits until a script expression is truthy and
//!   reads a JSON result out of the page ([`PageRequest`], [`PageOutcome`]).
//! - [`BrowserSession`] keeps one browser up with one cookie jar, the way a workspace's editor
//!   window keeps its data folder: cookies go in before a page opens ([`BrowserCookie`]), pages
//!   stay open, take keys and close like windows, and a page opened later finds the cookies.
//!
//! Chromium and its driver come from the UI's lockfile: `ui/package-lock.json` pins
//! `@playwright/test`, whose `playwright-core` drives the browser build Playwright's cache already
//! holds for the UI jobs. `PUDDLE_VM_BROWSER_MODULES` names the `ui/node_modules` folder
//! (installed with `npm ci --ignore-scripts`; the browser with `npx playwright install chromium`).
//! No second package, pin or download.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

use crate::driver::Driver;
use crate::error::HarnessError;

/// Environment variable naming the `node_modules` folder that holds `playwright-core`.
pub const BROWSER_MODULES_VAR: &str = "PUDDLE_VM_BROWSER_MODULES";

/// Environment variable naming the node binary; `node` from `PATH` when unset.
pub const NODE_VAR: &str = "PUDDLE_VM_NODE";

/// How long node gets beyond a page's own budget to close the browser and print its answer.
const NODE_GRACE: Duration = Duration::from_secs(20);

/// The browser half of the client, run by node (`browser/driver.mjs`).
const DRIVER: &str = include_str!("../browser/driver.mjs");

/// What a node process printed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeOutput {
    /// Whether it exited with status 0.
    pub success: bool,
    /// Standard output, lossily decoded.
    pub stdout: String,
    /// Standard error, lossily decoded.
    pub stderr: String,
}

/// A node binary that VM tests run scripts with.
#[derive(Debug, Clone)]
pub struct Node {
    pub(crate) program: OsString,
}

impl Node {
    /// The binary from `PUDDLE_VM_NODE`, or `node` from `PATH`.
    #[must_use]
    pub fn from_env() -> Self {
        Self::new(std::env::var_os(NODE_VAR).unwrap_or_else(|| "node".into()))
    }

    /// A specific binary.
    #[must_use]
    pub fn new(program: impl Into<OsString>) -> Self {
        Self {
            program: program.into(),
        }
    }

    /// `node <args>` with every standard stream piped, killed when dropped.
    pub(crate) fn command<I, S>(&self, args: I) -> Command
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let mut command = Command::new(&self.program);
        command
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        command
    }

    /// Runs `node <args>` with `stdin` written to it, and waits at most `budget` for it to end.
    /// The process is killed when the budget runs out.
    ///
    /// # Errors
    ///
    /// [`HarnessError::Io`] when node can't be started, [`HarnessError::NodeHung`] when the budget
    /// runs out (it says what node had printed and which steps had finished). A non-zero exit is not an error here: it is [`NodeOutput::success`].
    pub async fn run<I, S>(
        &self,
        args: I,
        envs: &[(&str, &OsStr)],
        stdin: &str,
        budget: Duration,
    ) -> Result<NodeOutput, HarnessError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let mut command = self.command(args);
        command.envs(envs.iter().copied());
        let program = PathBuf::from(&self.program);
        let spawn_began = Instant::now();
        let mut child = command
            .spawn()
            .map_err(|source| HarnessError::io("start node", &program, source))?;
        let spawn_took = spawn_began.elapsed();
        let pid = child.id();
        let progress = Arc::new(Progress::default());
        // A script that never reads its input closes the pipe early: that is not this call's error.
        let feed = child.stdin.take().map(|mut pipe| {
            let input = stdin.to_owned();
            let progress = Arc::clone(&progress);
            tokio::spawn(async move {
                let _ = pipe.write_all(input.as_bytes()).await;
                progress.stdin_done.store(true, Ordering::SeqCst);
            })
        });
        let out = child
            .stdout
            .take()
            .map(|pipe| progress.drain(pipe, Stream::Out));
        let err = child
            .stderr
            .take()
            .map(|pipe| progress.drain(pipe, Stream::Err));
        let waited = tokio::time::timeout(budget, async {
            let status = child.wait().await;
            progress.exited.store(true, Ordering::SeqCst);
            // The output ends when every holder of the pipe's write end has closed it: a process
            // node started that outlives it keeps the pipe open past node's own exit.
            for reader in [out, err].into_iter().flatten() {
                let _ = reader.await;
            }
            status
        })
        .await;
        if let Some(feed) = feed {
            feed.abort();
        }
        let Ok(status) = waited else {
            let stage = progress.stage();
            let _ = child.start_kill();
            return Err(HarnessError::NodeHung {
                limit: budget,
                pid,
                spawn_took,
                stage,
                stdout: progress.text(Stream::Out),
                stderr: progress.text(Stream::Err),
            });
        };
        let status =
            status.map_err(|source| HarnessError::io("wait for node", &program, source))?;
        Ok(NodeOutput {
            success: status.success(),
            stdout: progress.text(Stream::Out),
            stderr: progress.text(Stream::Err),
        })
    }
}

#[derive(Clone, Copy)]
enum Stream {
    Out,
    Err,
}

/// What a running node process has done so far: the output read from each pipe and which steps
/// finished, so a timeout can report both.
#[derive(Default)]
struct Progress {
    stdin_done: AtomicBool,
    exited: AtomicBool,
    out_closed: AtomicBool,
    err_closed: AtomicBool,
    out: Mutex<Vec<u8>>,
    err: Mutex<Vec<u8>>,
}

impl Progress {
    /// Reads `pipe` to its end into the buffer, on its own task.
    fn drain<R>(self: &Arc<Self>, mut pipe: R, stream: Stream) -> tokio::task::JoinHandle<()>
    where
        R: AsyncReadExt + Unpin + Send + 'static,
    {
        let this = Arc::clone(self);
        tokio::spawn(async move {
            let mut chunk = [0_u8; 4096];
            while let Ok(read) = pipe.read(&mut chunk).await {
                let Some(got) = chunk.get(..read).filter(|got| !got.is_empty()) else {
                    break;
                };
                this.buffer(stream)
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .extend_from_slice(got);
            }
            match stream {
                Stream::Out => this.out_closed.store(true, Ordering::SeqCst),
                Stream::Err => this.err_closed.store(true, Ordering::SeqCst),
            }
        })
    }

    fn buffer(&self, stream: Stream) -> &Mutex<Vec<u8>> {
        match stream {
            Stream::Out => &self.out,
            Stream::Err => &self.err,
        }
    }

    fn text(&self, stream: Stream) -> String {
        let bytes = self
            .buffer(stream)
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        String::from_utf8_lossy(&bytes).into_owned()
    }

    fn stage(&self) -> String {
        let flag = |done: &AtomicBool| {
            if done.load(Ordering::SeqCst) {
                "done"
            } else {
                "pending"
            }
        };
        format!(
            "stdin written: {}, process exited: {}, stdout closed: {}, stderr closed: {}",
            flag(&self.stdin_done),
            flag(&self.exited),
            flag(&self.out_closed),
            flag(&self.err_closed),
        )
    }
}

/// One page to open in the browser, or to wait on once it is open.
#[derive(Debug, Clone)]
pub struct PageRequest {
    url: String,
    ready: String,
    result: String,
    budget: Duration,
    ignore_https_errors: bool,
    loaded: bool,
}

impl PageRequest {
    /// Opens `url` and waits until the script expression `ready` is truthy in the page.
    /// The result defaults to `null`; the budget to 60 seconds.
    #[must_use]
    pub fn new(url: impl Into<String>, ready: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            ready: ready.into(),
            result: "null".to_owned(),
            budget: Duration::from_secs(60),
            ignore_https_errors: false,
            loaded: true,
        }
    }

    /// The script expression whose JSON value is returned once the page is ready.
    #[must_use]
    pub fn result(mut self, expression: impl Into<String>) -> Self {
        self.result = expression.into();
        self
    }

    /// How long loading the page and waiting for `ready` may take. Name why in the caller: it is
    /// a limit on a hang, never a bar on speed.
    #[must_use]
    pub fn budget(mut self, budget: Duration) -> Self {
        self.budget = budget;
        self
    }

    /// Waits for `ready` as soon as the page's navigation has committed (its answer came), not
    /// once its document has loaded: the moment a window exists, which is all a launcher waits for.
    #[must_use]
    pub fn on_commit(mut self) -> Self {
        self.loaded = false;
        self
    }

    /// Accept certificates the browser doesn't trust (a front with a private CA). Applies to the
    /// whole browser: [`BrowserClient::open`] honours it, a [`BrowserSession`] takes it at its start.
    #[must_use]
    pub fn ignore_https_errors(mut self) -> Self {
        self.ignore_https_errors = true;
        self
    }

    fn command(&self, op: &str, page: u32) -> Value {
        json!({
            "op": op,
            "page": page,
            "url": self.url,
            "ready": self.ready,
            "result": self.result,
            "timeoutMs": u64::try_from(self.budget.as_millis()).unwrap_or(u64::MAX),
            "waitUntil": if self.loaded { "domcontentloaded" } else { "commit" },
        })
    }
}

/// What the page gave back.
#[derive(Debug, Clone, PartialEq)]
pub struct PageOutcome {
    /// The value of the request's `result` expression.
    pub result: Value,
    /// The page's console lines since the last answer about it, as `type: text`.
    pub console: Vec<String>,
    /// Uncaught errors in the page since the last answer about it.
    pub page_errors: Vec<String>,
}

/// A cookie put in the browser before a page opens, the way a shell sets a window's sign-in
/// before it navigates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserCookie {
    name: String,
    value: String,
    domain: String,
    http_only: bool,
}

impl BrowserCookie {
    /// A host-only, `HttpOnly`, `SameSite=Lax` cookie for `domain` on path `/`.
    #[must_use]
    pub fn host_only(
        name: impl Into<String>,
        value: impl Into<String>,
        domain: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            value: value.into(),
            domain: domain.into(),
            http_only: true,
        }
    }

    /// The same cookie without `HttpOnly`: the page's script reads it.
    #[must_use]
    pub fn script_reads(mut self) -> Self {
        self.http_only = false;
        self
    }

    fn to_json(&self) -> Value {
        // Playwright makes a cookie host-only when it gets `url` instead of `domain`.
        json!({
            "name": self.name,
            "value": self.value,
            "url": format!("http://{}/", self.domain),
            "httpOnly": self.http_only,
            "sameSite": "Lax",
        })
    }
}

/// Headless Chromium driven from node.
#[derive(Debug, Clone)]
pub struct BrowserClient {
    node: Node,
    modules: PathBuf,
    /// How long node gets beyond a step's own budget ([`NODE_GRACE`]; shorter in this file's tests).
    grace: Duration,
}

impl BrowserClient {
    /// The client the environment describes: `PUDDLE_VM_NODE` and `PUDDLE_VM_BROWSER_MODULES`.
    ///
    /// # Errors
    ///
    /// [`HarnessError::MissingVar`] when the modules folder is not named,
    /// [`HarnessError::BrowserModules`] when it holds no `playwright-core`.
    pub fn from_env() -> Result<Self, HarnessError> {
        Self::from_vars(
            std::env::var_os(NODE_VAR),
            std::env::var_os(BROWSER_MODULES_VAR),
        )
    }

    fn from_vars(node: Option<OsString>, modules: Option<OsString>) -> Result<Self, HarnessError> {
        let modules = modules.ok_or(HarnessError::MissingVar {
            var: BROWSER_MODULES_VAR,
            hint: "ui/node_modules after `npm ci --ignore-scripts` in ui",
        })?;
        let node = node.map_or_else(Node::from_env, Node::new);
        Self::new(node, PathBuf::from(modules))
    }

    /// A client over a node binary and a `node_modules` folder.
    ///
    /// # Errors
    ///
    /// [`HarnessError::BrowserModules`] when the folder holds no `playwright-core`.
    pub fn new(node: Node, modules: PathBuf) -> Result<Self, HarnessError> {
        if !has_playwright(&modules) {
            return Err(HarnessError::BrowserModules { dir: modules });
        }
        Ok(Self {
            node,
            modules,
            grace: NODE_GRACE,
        })
    }

    /// Opens the page in a browser of its own, waits for it to be ready, reads its result and
    /// closes the browser.
    ///
    /// # Errors
    ///
    /// [`HarnessError::Browser`] when the page never became ready or the driver failed (the
    /// message carries the page's console and errors), [`HarnessError::Timeout`] when node itself
    /// hung.
    pub async fn open(&self, request: &PageRequest) -> Result<PageOutcome, HarnessError> {
        let mut session = self.session_with(request.ignore_https_errors).await?;
        let opened = session.open(request).await;
        session.quit().await?;
        opened.map(|(_, outcome)| outcome)
    }

    /// Starts a browser that stays up until [`BrowserSession::quit`]: pages open and close in it
    /// like the windows of one data folder, sharing its cookies.
    ///
    /// # Errors
    ///
    /// [`HarnessError::Io`] when node can't be started, [`HarnessError::Browser`] when the browser
    /// did not start.
    pub async fn session(&self) -> Result<BrowserSession, HarnessError> {
        self.session_with(false).await
    }

    async fn session_with(
        &self,
        ignore_https_errors: bool,
    ) -> Result<BrowserSession, HarnessError> {
        let driver = Driver::spawn(
            &self.node,
            DRIVER,
            &[(BROWSER_MODULES_VAR, self.modules.as_os_str())],
            self.grace,
            "browser client",
            "closing the browser",
        )?;
        let mut session = BrowserSession {
            driver,
            next_page: 0,
        };
        session
            .call(
                &json!({"op": "start", "ignoreHttpsErrors": ignore_https_errors}),
                START_BUDGET,
            )
            .await?;
        Ok(session)
    }
}

/// How long Chromium may take to start (a cold start on a loaded runner takes seconds).
const START_BUDGET: Duration = Duration::from_secs(60);

/// A page open in a [`BrowserSession`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageId(u32);

/// One running browser with one cookie jar; see [`BrowserClient::session`]. Dropping it kills the
/// browser; [`BrowserSession::quit`] closes it cleanly.
#[derive(Debug)]
pub struct BrowserSession {
    driver: Driver,
    next_page: u32,
}

impl BrowserSession {
    /// Adds cookies to the browser, for the pages opened after.
    ///
    /// # Errors
    ///
    /// [`HarnessError::Browser`] when the browser refused them.
    pub async fn add_cookies(&mut self, cookies: &[BrowserCookie]) -> Result<(), HarnessError> {
        let cookies: Vec<Value> = cookies.iter().map(BrowserCookie::to_json).collect();
        self.call(&json!({"op": "cookies", "cookies": cookies}), START_BUDGET)
            .await
            .map(drop)
    }

    /// Opens a new page, which stays open, and waits for it as [`BrowserClient::open`] does.
    ///
    /// # Errors
    ///
    /// As [`BrowserClient::open`]; the page stays open after a failure and can be waited on again.
    pub async fn open(
        &mut self,
        request: &PageRequest,
    ) -> Result<(PageId, PageOutcome), HarnessError> {
        let page = PageId(self.next_page);
        self.next_page += 1;
        let outcome = self
            .call(&request.command("open", page.0), request.budget)
            .await?;
        Ok((page, outcome))
    }

    /// Waits until the request's `ready` is truthy in `page` (its URL is not used) and reads its
    /// `result`.
    ///
    /// # Errors
    ///
    /// As [`BrowserClient::open`].
    pub async fn wait(
        &mut self,
        page: PageId,
        request: &PageRequest,
    ) -> Result<PageOutcome, HarnessError> {
        self.call(&request.command("wait", page.0), request.budget)
            .await
    }

    /// Presses `keys` in `page` (Playwright's names, such as `Control+Backquote`).
    ///
    /// # Errors
    ///
    /// [`HarnessError::Browser`] when the page is not open.
    pub async fn press(&mut self, page: PageId, keys: &str) -> Result<(), HarnessError> {
        self.call(
            &json!({"op": "press", "page": page.0, "keys": keys}),
            START_BUDGET,
        )
        .await
        .map(drop)
    }

    /// Closes `page` as a closed window does: its connections end.
    ///
    /// # Errors
    ///
    /// [`HarnessError::Browser`] when the page is not open.
    pub async fn close(&mut self, page: PageId) -> Result<(), HarnessError> {
        self.call(&json!({"op": "close", "page": page.0}), START_BUDGET)
            .await
            .map(drop)
    }

    /// Closes the browser and waits for node to end.
    ///
    /// # Errors
    ///
    /// [`HarnessError::Timeout`] when it did not end in time (it is killed then).
    pub async fn quit(self) -> Result<(), HarnessError> {
        self.driver.quit().await
    }

    async fn call(
        &mut self,
        command: &Value,
        budget: Duration,
    ) -> Result<PageOutcome, HarnessError> {
        self.driver.call(command, budget).await
    }
}

/// Whether `dir` looks like a `node_modules` folder with Playwright's driver.
fn has_playwright(dir: &Path) -> bool {
    dir.join("playwright-core").join("package.json").is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_request_carries_the_page_and_the_budget_in_milliseconds() {
        let request = PageRequest::new("http://a.localhost:1/", "window.done")
            .result("window.value")
            .budget(Duration::from_secs(5));
        let parsed = request.command("wait", 7);
        assert_eq!(parsed["op"], "wait");
        assert_eq!(parsed["page"], 7);
        assert_eq!(parsed["timeoutMs"], 5000);
        assert_eq!(parsed["ready"], "window.done");
        assert_eq!(parsed["result"], "window.value");
        assert_eq!(parsed["waitUntil"], "domcontentloaded");
        assert_eq!(
            request.on_commit().command("open", 0)["waitUntil"],
            "commit"
        );
    }

    #[test]
    fn a_cookie_is_host_only_http_only_and_lax() {
        let cookie = BrowserCookie::host_only("s", "v", "demo.localhost:4800").to_json();
        assert_eq!(
            cookie,
            json!({"name": "s", "value": "v", "url": "http://demo.localhost:4800/", "httpOnly": true, "sameSite": "Lax"})
        );
        let readable = BrowserCookie::host_only("t", "v", "demo.localhost")
            .script_reads()
            .to_json();
        assert_eq!(readable["httpOnly"], false);
    }

    #[test]
    fn a_folder_without_playwright_is_refused_with_its_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let err = BrowserClient::new(Node::new("node"), dir.path().to_owned()).expect_err("empty");
        assert!(err.to_string().contains(&dir.path().display().to_string()));
        assert!(!has_playwright(dir.path()));
        std::fs::create_dir_all(dir.path().join("playwright-core")).expect("mkdir");
        std::fs::write(dir.path().join("playwright-core/package.json"), "{}").expect("write");
        assert!(has_playwright(dir.path()));
        BrowserClient::new(Node::new("node"), dir.path().to_owned()).expect("now present");
    }

    #[tokio::test]
    async fn node_runs_a_script_with_stdin_env_and_reports_its_exit() {
        let node = Node::from_env();
        let got = node
            .run(
                [
                    "-e",
                    "let s='';process.stdin.on('data',d=>s+=d).on('end',()=>{console.log(s+process.env.VM_NODE_PROBE);process.exit(3)})",
                ],
                &[("VM_NODE_PROBE", OsStr::new("env"))],
                "in-",
                Duration::from_secs(30),
            )
            .await
            .expect("node runs");
        assert!(!got.success);
        assert_eq!(got.stdout.trim(), "in-env");
    }

    #[tokio::test]
    async fn node_that_hangs_is_stopped_at_the_budget() {
        let err = Node::from_env()
            .run(
                ["-e", "setInterval(()=>{},1000)"],
                &[],
                "",
                Duration::from_secs(2),
            )
            .await
            .expect_err("hang");
        assert!(matches!(err, HarnessError::NodeHung { .. }), "{err}");
    }

    #[tokio::test]
    async fn a_hung_node_reports_what_it_printed_and_how_far_it_got() {
        let err = Node::from_env()
            .run(
                [
                    "-e",
                    "console.log('out-line'); console.error('err-line'); setInterval(()=>{},1000)",
                ],
                &[],
                "",
                Duration::from_secs(3),
            )
            .await
            .expect_err("hang");
        let said = err.to_string();
        assert!(matches!(err, HarnessError::NodeHung { .. }), "{said}");
        for part in [
            "stdout: \"out-line",
            "stderr: \"err-line",
            "stdin written: done, process exited: pending, stdout closed: pending, stderr closed: pending",
        ] {
            assert!(said.contains(part), "{part} not in {said}");
        }
    }

    #[tokio::test]
    async fn a_missing_node_binary_names_the_path() {
        let err = Node::new("/nonexistent/node-probe")
            .run(["-v"], &[], "", Duration::from_secs(5))
            .await
            .expect_err("no binary");
        assert!(err.to_string().contains("node-probe"), "{err}");
        let err = BrowserClient {
            node: Node::new("/nonexistent/node-probe"),
            // flake-ok: a path that must not exist; on Windows it is rooted on the current drive and still does not exist
            modules: PathBuf::from("/nonexistent"),
            grace: NODE_GRACE,
        }
        .session()
        .await
        .expect_err("no binary");
        assert!(err.to_string().contains("node-probe"), "{err}");
    }

    /// A `playwright-core` that drives no browser. Its page answers a wait with the request's
    /// `result` expression and what it knows (the URL it went to, the cookies of its context, the
    /// keys pressed); `ready` of `never` times out; it logs every `goto`, and a `goto` to a URL
    /// ending in `/hang` never ends. A cookie named `exit` ends node. `launch` fails when the
    /// variable `STUB_NO_LAUNCH` is set.
    fn stub_modules() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let core = dir.path().join("node_modules").join("playwright-core");
        std::fs::create_dir_all(&core).expect("mkdir");
        std::fs::write(core.join("package.json"), "{\"main\":\"index.js\"}").expect("package");
        std::fs::write(
            core.join("index.js"),
            "exports.chromium = { launch: async () => {
               if (process.env.STUB_NO_LAUNCH) throw new Error('no browser here');
               return {
               newContext: async (o) => {
                 const jar = [];
                 return {
                 addCookies: async (c) => { if (c.some((k) => k.name === 'exit')) process.exit(3); jar.push(...c); },
                 newPage: async () => {
                   const h = {}; let url = null; const keys = [];
                   return {
                     on: (e, f) => { h[e] = f; },
                     goto: async (u) => { if (u.endsWith('/hang')) await new Promise(() => {}); url = u; h.console({ type: () => 'log', text: () => 'at ' + u }); },
                     waitForFunction: async (e) => { if (e === 'never') { h.pageerror(new Error('never ready')); throw new Error('timed out'); } },
                     evaluate: async (e) => ({ evaluated: e, url, cookies: jar.map(c => c.name), keys, strict: o.ignoreHTTPSErrors }),
                     keyboard: { press: async (k) => { keys.push(k); } },
                     close: async () => {},
                   };
                 } };
               },
               close: async () => {},
             }; } };",
        )
        .expect("index");
        dir
    }

    fn stub_client(modules: &tempfile::TempDir) -> BrowserClient {
        BrowserClient::new(Node::from_env(), modules.path().join("node_modules")).expect("client")
    }

    #[tokio::test]
    async fn the_driver_opens_the_page_waits_and_returns_the_result_with_the_console() {
        let modules = stub_modules();
        let request = PageRequest::new("http://x.localhost:9/", "ready")
            .result("answer")
            .ignore_https_errors();
        let outcome = stub_client(&modules).open(&request).await.expect("opened");
        assert_eq!(
            outcome.result,
            json!({"evaluated": "answer", "url": "http://x.localhost:9/", "cookies": [], "keys": [], "strict": true})
        );
        assert_eq!(outcome.console, ["log: at http://x.localhost:9/"]);
    }

    #[tokio::test]
    async fn the_driver_reports_a_page_that_never_becomes_ready() {
        let modules = stub_modules();
        let err = stub_client(&modules)
            .open(&PageRequest::new("http://x.localhost:9/", "never"))
            .await
            .expect_err("not ready");
        let text = err.to_string();
        assert!(text.contains("timed out"), "{text}");
        assert!(text.contains("at http://x.localhost:9/"), "{text}");
        assert!(text.contains("never ready"), "{text}");
    }

    #[tokio::test]
    async fn a_session_keeps_its_cookies_and_pages_takes_keys_and_closes_them() {
        let modules = stub_modules();
        let mut session = stub_client(&modules).session().await.expect("session");
        session
            .add_cookies(&[BrowserCookie::host_only("sign-in", "v", "x.localhost:9")])
            .await
            .expect("cookies");
        let (first, opened) = session
            .open(&PageRequest::new("http://x.localhost:9/a", "ready").result("r"))
            .await
            .expect("first page");
        assert_eq!(opened.result["cookies"], json!(["sign-in"]));
        assert_eq!(opened.result["strict"], false);
        let (second, _) = session
            .open(&PageRequest::new("http://x.localhost:9/b", "ready"))
            .await
            .expect("second page");
        assert_ne!(first, second);
        session
            .press(first, "Control+Backquote")
            .await
            .expect("keys");
        let waited = session
            .wait(first, &PageRequest::new("", "ready").result("r"))
            .await
            .expect("wait");
        // The first page still is where it went, has the key, and logged nothing new.
        assert_eq!(waited.result["url"], "http://x.localhost:9/a");
        assert_eq!(waited.result["keys"], json!(["Control+Backquote"]));
        assert!(waited.console.is_empty(), "{:?}", waited.console);
        session.close(first).await.expect("close");
        let err = session
            .wait(first, &PageRequest::new("", "ready"))
            .await
            .expect_err("closed");
        assert!(err.to_string().contains("no open page 0"), "{err}");
        let err = session.press(first, "a").await.expect_err("closed");
        assert!(err.to_string().contains("no open page"), "{err}");
        let err = session.close(first).await.expect_err("closed twice");
        assert!(err.to_string().contains("no open page"), "{err}");
        session.close(second).await.expect("close the second");
        session.quit().await.expect("quit");
    }

    /// Unix only: the stand-in for node is a shell script.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_browser_that_does_not_start_says_why() {
        let modules = stub_modules();
        // The variable reaches only the driver's process, through a wrapper around node.
        let node = Node::from_env().program;
        let wrapper = modules.path().join("no-launch.sh");
        puddle_test_kit::write_executable(
            &wrapper,
            &format!(
                "#!/bin/sh\nSTUB_NO_LAUNCH=1 exec '{}' \"$@\"\n",
                node.to_string_lossy()
            ),
        );
        let err = BrowserClient::new(Node::new(&wrapper), modules.path().join("node_modules"))
            .expect("client")
            .session()
            .await
            .expect_err("no browser");
        assert!(err.to_string().contains("no browser here"), "{err}");
    }

    /// Unix only: the stand-in for node is a shell script.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_driver_that_dies_says_so_with_its_stderr() {
        let modules = stub_modules();
        let wrapper = modules.path().join("dies.sh");
        puddle_test_kit::write_executable(
            &wrapper,
            "#!/bin/sh\necho 'Cannot find module' >&2\nexit 1\n",
        );
        let err = BrowserClient::new(Node::new(&wrapper), modules.path().join("node_modules"))
            .expect("client")
            .session()
            .await
            .expect_err("dead driver");
        let text = err.to_string();
        assert!(text.contains("Cannot find module"), "{text}");
    }

    #[tokio::test]
    async fn a_step_that_hangs_and_a_browser_that_does_not_close_are_given_up_on() {
        let modules = stub_modules();
        let mut client = stub_client(&modules);
        client.grace = Duration::from_millis(500);
        let mut session = client.session().await.expect("session");
        let err = session
            .open(
                &PageRequest::new("http://x.localhost:9/hang", "ready")
                    .budget(Duration::from_millis(1)),
            )
            .await
            .expect_err("hung");
        assert!(
            matches!(
                err,
                HarnessError::Timeout {
                    what: "browser client",
                    ..
                }
            ),
            "{err}"
        );
        // The driver is still in the hung step, so it never reads the quit.
        let err = session.quit().await.expect_err("stuck");
        assert!(
            matches!(
                err,
                HarnessError::Timeout {
                    what: "closing the browser",
                    ..
                }
            ),
            "{err}"
        );
    }

    #[tokio::test]
    async fn a_driver_that_ends_mid_session_says_so_and_is_gone_for_the_next_step() {
        let modules = stub_modules();
        let mut session = stub_client(&modules).session().await.expect("session");
        let err = session
            .add_cookies(&[BrowserCookie::host_only("exit", "v", "x.localhost")])
            .await
            .expect_err("node ended");
        assert!(err.to_string().contains("ended without an answer"), "{err}");
        // Its stderr was read with the first failure; the next step fails without it.
        let err = session.press(PageId(0), "a").await.expect_err("no driver");
        assert!(err.to_string().contains("ended without an answer"), "{err}");
    }

    #[test]
    fn the_client_needs_the_modules_variable_and_takes_a_node_override() {
        let missing = BrowserClient::from_vars(None, None).expect_err("no modules");
        assert!(
            missing.to_string().contains(BROWSER_MODULES_VAR),
            "{missing}"
        );
        let modules = stub_modules();
        BrowserClient::from_vars(
            Some("some-node".into()),
            Some(modules.path().join("node_modules").into_os_string()),
        )
        .expect("with a node override");
        // The process's own environment: either answer is right, the line must run.
        let _ = BrowserClient::from_env();
    }
}
