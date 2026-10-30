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
//!
//! Chromium and its driver come from the UI's lockfile: `ui/package-lock.json` pins
//! `@playwright/test`, whose `playwright-core` drives the browser build Playwright's cache already
//! holds for the UI jobs. `PUDDLE_VM_BROWSER_MODULES` names the `ui/node_modules` folder
//! (installed with `npm ci --ignore-scripts`; the browser with `npx playwright install chromium`).
//! No second package, pin or download.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

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
    program: OsString,
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

    /// Runs `node <args>` with `stdin` written to it, and waits at most `budget` for it to end.
    /// The process is killed when the budget runs out.
    ///
    /// # Errors
    ///
    /// [`HarnessError::Io`] when node can't be started, [`HarnessError::Timeout`] when the budget
    /// runs out. A non-zero exit is not an error here: it is [`NodeOutput::success`].
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
        let mut command = Command::new(&self.program);
        command
            .args(args)
            .envs(envs.iter().copied())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let program = PathBuf::from(&self.program);
        let mut child = command
            .spawn()
            .map_err(|source| HarnessError::io("start node", &program, source))?;
        // A script that never reads its input closes the pipe early: that is not this call's error.
        let feed = child.stdin.take().map(|mut pipe| {
            let input = stdin.to_owned();
            tokio::spawn(async move {
                let _ = pipe.write_all(input.as_bytes()).await;
            })
        });
        let waited = tokio::time::timeout(budget, child.wait_with_output()).await;
        if let Some(feed) = feed {
            feed.abort();
        }
        let output = waited
            .map_err(|_| HarnessError::Timeout {
                what: "node process",
                limit: budget,
            })?
            .map_err(|source| HarnessError::io("wait for node", &program, source))?;
        Ok(NodeOutput {
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

/// One page to open in the browser.
#[derive(Debug, Clone)]
pub struct PageRequest {
    url: String,
    ready: String,
    result: String,
    budget: Duration,
    ignore_https_errors: bool,
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

    /// Accept certificates the browser doesn't trust (a front with a private CA).
    #[must_use]
    pub fn ignore_https_errors(mut self) -> Self {
        self.ignore_https_errors = true;
        self
    }

    fn to_json(&self) -> String {
        json!({
            "url": self.url,
            "ready": self.ready,
            "result": self.result,
            "timeoutMs": u64::try_from(self.budget.as_millis()).unwrap_or(u64::MAX),
            "ignoreHttpsErrors": self.ignore_https_errors,
        })
        .to_string()
    }
}

/// What the page gave back.
#[derive(Debug, Clone, PartialEq)]
pub struct PageOutcome {
    /// The value of the request's `result` expression.
    pub result: Value,
    /// The page's console lines, as `type: text`.
    pub console: Vec<String>,
    /// Uncaught errors in the page.
    pub page_errors: Vec<String>,
}

/// Headless Chromium driven from node.
#[derive(Debug, Clone)]
pub struct BrowserClient {
    node: Node,
    modules: PathBuf,
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
        Ok(Self { node, modules })
    }

    /// Opens the page, waits for it to be ready and reads its result.
    ///
    /// # Errors
    ///
    /// [`HarnessError::Browser`] when the page never became ready or the driver failed (the
    /// message carries the page's console and errors), [`HarnessError::Timeout`] when node itself
    /// hung.
    pub async fn open(&self, request: &PageRequest) -> Result<PageOutcome, HarnessError> {
        let output = self
            .node
            .run(
                ["--input-type=module", "-e", DRIVER],
                &[(BROWSER_MODULES_VAR, self.modules.as_os_str())],
                &request.to_json(),
                request.budget + NODE_GRACE,
            )
            .await?;
        parse_reply(&output)
    }
}

/// Reads the driver's one-line answer.
fn parse_reply(output: &NodeOutput) -> Result<PageOutcome, HarnessError> {
    let fail = |detail: String| HarnessError::Browser { detail };
    let line = output
        .stdout
        .lines()
        .rev()
        .find(|line| line.starts_with('{'))
        .ok_or_else(|| {
            fail(format!(
                "the driver printed no answer (exit ok: {}); stderr: {}",
                output.success,
                output.stderr.trim()
            ))
        })?;
    let reply: Value =
        serde_json::from_str(line).map_err(|e| fail(format!("unreadable driver answer: {e}")))?;
    let field = |key: &str| reply.get(key).unwrap_or(&Value::Null);
    let strings = |key: &str| -> Vec<String> {
        field(key)
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect()
    };
    let console = strings("console");
    let page_errors = strings("pageErrors");
    if field("ok").as_bool() != Some(true) {
        return Err(fail(format!(
            "{}; console: {console:?}; page errors: {page_errors:?}",
            field("error").as_str().unwrap_or("no reason given")
        )));
    }
    Ok(PageOutcome {
        result: field("result").clone(),
        console,
        page_errors,
    })
}

/// Whether `dir` looks like a `node_modules` folder with Playwright's driver.
fn has_playwright(dir: &Path) -> bool {
    dir.join("playwright-core").join("package.json").is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn out(stdout: &str) -> NodeOutput {
        NodeOutput {
            success: true,
            stdout: stdout.to_owned(),
            stderr: String::new(),
        }
    }

    #[test]
    fn a_ready_page_gives_its_result_and_its_console() {
        let outcome = parse_reply(&out(
            "noise\n{\"ok\":true,\"result\":{\"n\":3},\"console\":[\"log: hi\"],\"pageErrors\":[]}\n",
        ))
        .expect("answer");
        assert_eq!(outcome.result, json!({"n": 3}));
        assert_eq!(outcome.console, ["log: hi"]);
        assert_eq!(outcome.page_errors, Vec::<String>::new());
    }

    #[test]
    fn a_failed_page_names_the_reason_the_console_and_the_page_errors() {
        let err = parse_reply(&out(
            "{\"ok\":false,\"error\":\"waiting failed\",\"console\":[\"error: boom\"],\"pageErrors\":[\"Error: x\"]}",
        ))
        .expect_err("not ok");
        let text = err.to_string();
        assert!(text.contains("waiting failed"), "{text}");
        assert!(text.contains("error: boom"), "{text}");
        assert!(text.contains("Error: x"), "{text}");
    }

    #[test]
    fn a_driver_that_printed_nothing_shows_its_stderr() {
        let err = parse_reply(&NodeOutput {
            success: false,
            stdout: String::new(),
            stderr: "Cannot find module\n".to_owned(),
        })
        .expect_err("no answer");
        assert!(err.to_string().contains("Cannot find module"), "{err}");
    }

    #[test]
    fn an_unreadable_answer_is_an_error_not_a_pass() {
        let err = parse_reply(&out("{not json")).expect_err("bad json");
        assert!(err.to_string().contains("unreadable"), "{err}");
    }

    #[test]
    fn the_request_carries_the_budget_in_milliseconds() {
        let request = PageRequest::new("http://a.localhost:1/", "window.done")
            .result("window.value")
            .budget(Duration::from_secs(5))
            .ignore_https_errors();
        let parsed: Value = serde_json::from_str(&request.to_json()).expect("json");
        assert_eq!(parsed["timeoutMs"], 5000);
        assert_eq!(parsed["ready"], "window.done");
        assert_eq!(parsed["result"], "window.value");
        assert_eq!(parsed["ignoreHttpsErrors"], true);
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
        assert!(matches!(err, HarnessError::Timeout { .. }), "{err}");
    }

    #[tokio::test]
    async fn a_missing_node_binary_names_the_path() {
        let err = Node::new("/nonexistent/node-probe")
            .run(["-v"], &[], "", Duration::from_secs(5))
            .await
            .expect_err("no binary");
        assert!(err.to_string().contains("node-probe"), "{err}");
    }

    /// A `playwright-core` that drives no browser: `launch` returns a page whose answers the
    /// request's own expressions select, so the driver's flow runs without Chromium.
    fn stub_modules() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let core = dir.path().join("node_modules").join("playwright-core");
        std::fs::create_dir_all(&core).expect("mkdir");
        std::fs::write(core.join("package.json"), "{\"main\":\"index.js\"}").expect("package");
        std::fs::write(
            core.join("index.js"),
            "exports.chromium = { launch: async () => ({
               newContext: async (o) => ({ newPage: async () => {
                 const h = {};
                 return {
                   on: (e, f) => { h[e] = f; },
                   goto: async (u) => { h.console({ type: () => 'log', text: () => 'at ' + u }); },
                   waitForFunction: async (e) => { if (e === 'never') throw new Error('timed out'); },
                   evaluate: async (e) => ({ evaluated: e, strict: o.ignoreHTTPSErrors }),
                 };
               } }),
               close: async () => {},
             }) };",
        )
        .expect("index");
        dir
    }

    #[tokio::test]
    async fn the_driver_opens_the_page_waits_and_returns_the_result_with_the_console() {
        let modules = stub_modules();
        let client = BrowserClient::new(Node::from_env(), modules.path().join("node_modules"))
            .expect("client");
        let request = PageRequest::new("http://x.localhost:9/", "ready")
            .result("answer")
            .ignore_https_errors();
        let outcome = client.open(&request).await.expect("opened");
        assert_eq!(
            outcome.result,
            json!({"evaluated": "answer", "strict": true})
        );
        assert_eq!(outcome.console, ["log: at http://x.localhost:9/"]);
    }

    #[tokio::test]
    async fn the_driver_reports_a_page_that_never_becomes_ready() {
        let modules = stub_modules();
        let client = BrowserClient::new(Node::from_env(), modules.path().join("node_modules"))
            .expect("client");
        let err = client
            .open(&PageRequest::new("http://x.localhost:9/", "never"))
            .await
            .expect_err("not ready");
        let text = err.to_string();
        assert!(text.contains("timed out"), "{text}");
        assert!(text.contains("at http://x.localhost:9/"), "{text}");
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
