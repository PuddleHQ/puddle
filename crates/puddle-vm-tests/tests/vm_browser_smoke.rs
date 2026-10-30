// SPDX-License-Identifier: GPL-3.0-or-later
//! Smoke for the VM tier's browser client: headless Chromium and node each load something over a
//! real loopback connection and read a result back. It boots no VM: the server is the test's own
//! (the browser editor tests put the host's front there). Linux only, because only `vm-linux`
//! installs the browser (docs of `puddle_vm_tests::BrowserClient`).
#![cfg(target_os = "linux")]

use std::time::Duration;

use puddle_vm_tests::{BrowserClient, Node, PageRequest};
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
