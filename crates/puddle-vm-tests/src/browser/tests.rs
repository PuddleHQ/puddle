// SPDX-License-Identifier: AGPL-3.0-or-later
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
