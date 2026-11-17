// SPDX-License-Identifier: AGPL-3.0-or-later
//! The kept-alive judge against a real curl on loopback, no VM: requests on a kept-alive route share
//! one connection and pass, and a route that closes after every answer fails the judge. Without the
//! second, a green nightly could mean the requests never shared a connection (`vm_perf_keepalive_*`
//! uses the same script and judge).
#![cfg(unix)]
#![expect(clippy::expect_used, reason = "test: a failed step fails the test")]

use puddle_vm_tests::BulkHost;
use puddle_vm_tests::keepalive::{Report, parse_requests, script};
use puddle_vm_tests::load::Route;

async fn run(url: &str, count: usize) -> Report {
    let out = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(script(url, count, Route::Direct, 60))
        .output()
        .await
        .expect("run sh");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    Report::new(count, parse_requests(&text).expect("curl's lines parse"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fifty_requests_on_a_kept_alive_route_share_one_connection_and_all_get_answered() {
    let host = BulkHost::start(1).await.expect("server");
    let report = run(&format!("http://{}/keepalive/16", host.addr()), 50).await;
    eprintln!("{}", report.summary());
    assert_eq!(report.problems(), Vec::<String>::new());
    assert_eq!(host.served(), 50);
    assert!(report.per_request_ms().expect("figure") > 0.0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_route_that_closes_after_every_answer_is_not_a_kept_alive_measurement() {
    let host = BulkHost::start(1).await.expect("server");
    let report = run(&format!("http://{}/gate/16", host.addr()), 5).await;
    let problems = report.problems();
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(
        problems[0].contains("5 connections were opened"),
        "{problems:?}"
    );
}
