// SPDX-License-Identifier: GPL-3.0-or-later
//! Smoke for the VM tier's browser client: headless Chromium and node each load something over a
//! real loopback connection and read a result back. It boots no VM: the server is the test's own
//! (the browser editor tests put the host's front there). Linux only, because only `vm-linux`
//! installs the browser (docs of `puddle_vm_tests::BrowserClient`).
#![cfg(target_os = "linux")]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use puddle_vm_tests::{BrowserClient, BrowserCookie, Node, PageRequest};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Only a limit on a hang: the page is served from this process and answers in milliseconds.
const PAGE_BUDGET: Duration = Duration::from_secs(60);

const PAGE: &str = "<!doctype html><title>start</title><script>\
fetch('/answer').then(r => r.json()).then(j => { window.answer = j; document.title = 'done'; });\
console.log('page loaded');\
</script>";
const ANSWER: &str = "{\"sum\":42,\"greeting\":\"hello from the server\"}";

/// A loopback server that gives the page on `/` and the answer on `/answer`. The listener is
/// bound before the address is given out and stays bound until the returned task is dropped.
async fn serve() -> std::io::Result<(String, tokio::task::JoinHandle<()>)> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let origin = format!("http://{}", listener.local_addr()?);
    let task = tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut head = vec![0_u8; 4096];
                let n = stream.read(&mut head).await.unwrap_or(0);
                let first = String::from_utf8_lossy(head.get(..n).unwrap_or_default());
                let (kind, body) = if first.starts_with("GET /answer ") {
                    ("application/json", ANSWER)
                } else if first.starts_with("GET / ") {
                    ("text/html", PAGE)
                } else {
                    ("text/plain", "")
                };
                let reply = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: {kind}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(reply.as_bytes()).await;
                let _ = stream.shutdown().await;
            });
        }
    });
    Ok((origin, task))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn vm_browser_chromium_loads_a_page_and_reads_its_result() {
    let client = BrowserClient::from_env().expect("browser client environment");
    let (origin, server) = serve().await.expect("loopback server");
    let request = PageRequest::new(format!("{origin}/"), "document.title === 'done'")
        .result("window.answer")
        .budget(PAGE_BUDGET);
    let outcome = client.open(&request).await.expect("the page became ready");
    server.abort();
    assert_eq!(
        outcome.result,
        json!({"sum": 42, "greeting": "hello from the server"})
    );
    assert!(
        outcome.console.contains(&"log: page loaded".to_owned()),
        "console: {:?}",
        outcome.console
    );
    assert!(outcome.page_errors.is_empty(), "{:?}", outcome.page_errors);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn vm_browser_a_page_that_never_becomes_ready_fails_with_what_it_logged() {
    let client = BrowserClient::from_env().expect("browser client environment");
    let (origin, server) = serve().await.expect("loopback server");
    // A short budget on purpose: the wait can never succeed, so only the failure text is under test.
    let request = PageRequest::new(format!("{origin}/"), "window.neverSet === true")
        .budget(Duration::from_secs(3));
    let err = client.open(&request).await.expect_err("never ready");
    server.abort();
    let text = err.to_string();
    assert!(
        text.contains("page loaded"),
        "the console is in the error: {text}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn vm_browser_node_fetches_from_the_server_and_prints_the_answer() {
    let (origin, server) = serve().await.expect("loopback server");
    let script = "const r = await fetch(process.argv[1] + '/answer'); console.log(JSON.stringify(await r.json()));";
    let output = Node::from_env()
        .run(
            ["--input-type=module", "-e", script, "--", origin.as_str()],
            &[],
            "",
            PAGE_BUDGET,
        )
        .await
        .expect("node ran");
    server.abort();
    assert!(output.success, "stderr: {}", output.stderr);
    let got: serde_json::Value = serde_json::from_str(output.stdout.trim()).expect("json");
    assert_eq!(got["sum"], 42);
}

/// A page that shows the `Cookie` header it was served with and then holds one request open,
/// as an editor window holds its socket.
fn holding_page(cookie: &str) -> String {
    format!(
        "<!doctype html><title>held</title><script>\
         window.cookie = {cookie:?};\
         fetch('/hold').catch(() => {{}});\
         window.holding = true;\
         </script>"
    )
}

/// A loopback server for [`holding_page`]: counts the `/hold` requests whose connection is still
/// open (the count drops when the browser ends the connection).
async fn serve_holding() -> std::io::Result<(u16, Arc<AtomicUsize>, tokio::task::JoinHandle<()>)> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let held = Arc::new(AtomicUsize::new(0));
    let count = held.clone();
    let task = tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let held = count.clone();
            tokio::spawn(async move {
                let mut head = vec![0_u8; 8192];
                let n = stream.read(&mut head).await.unwrap_or(0);
                let request =
                    String::from_utf8_lossy(head.get(..n).unwrap_or_default()).into_owned();
                if request.starts_with("GET /hold ") {
                    held.fetch_add(1, Ordering::SeqCst);
                    let _ = stream
                        .write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\ntransfer-encoding: chunked\r\n\r\n")
                        .await;
                    // Held until the browser ends the connection.
                    let mut rest = [0_u8; 512];
                    while matches!(stream.read(&mut rest).await, Ok(n) if n > 0) {}
                    held.fetch_sub(1, Ordering::SeqCst);
                    return;
                }
                let cookie = request
                    .lines()
                    .find_map(|line| {
                        line.strip_prefix("Cookie: ")
                            .or_else(|| line.strip_prefix("cookie: "))
                    })
                    .unwrap_or("")
                    .to_owned();
                let body = holding_page(&cookie);
                let reply = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/html\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(reply.as_bytes()).await;
                let _ = stream.shutdown().await;
            });
        }
    });
    Ok((port, held, task))
}

/// Waits until `held` is `want`; a limit on a hang only.
async fn held_becomes(held: &AtomicUsize, want: usize) {
    for _ in 0..300 {
        if held.load(Ordering::SeqCst) == want {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!(
        "held connections stayed {} (wanted {want})",
        held.load(Ordering::SeqCst)
    );
}

/// The session the browser editor tests use: a cookie set before the page opens reaches the
/// server on a `<name>.localhost` origin, an open page keeps its connection, closing the page ends
/// it, and a page opened again in the same session sends the cookie again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn vm_browser_a_session_keeps_its_cookie_and_a_closed_page_ends_its_connection() {
    let client = BrowserClient::from_env().expect("browser client environment");
    let (port, held, server) = serve_holding().await.expect("loopback server");
    let host = format!("smoke.localhost:{port}");
    let mut session = client.session().await.expect("browser started");
    session
        .add_cookies(&[BrowserCookie::host_only("sign-in", "kept", &host)])
        .await
        .expect("cookie");
    let request = PageRequest::new(format!("http://{host}/"), "window.holding === true")
        .result("window.cookie")
        .budget(PAGE_BUDGET);
    let (page, outcome) = session.open(&request).await.expect("the page opened");
    assert_eq!(outcome.result, json!("sign-in=kept"));
    held_becomes(&held, 1).await;
    session.close(page).await.expect("closed");
    held_becomes(&held, 0).await;
    let (_, again) = session.open(&request).await.expect("opened again");
    assert_eq!(again.result, json!("sign-in=kept"));
    held_becomes(&held, 1).await;
    session.quit().await.expect("browser closed");
    held_becomes(&held, 0).await;
    server.abort();
}
