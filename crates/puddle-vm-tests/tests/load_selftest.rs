// SPDX-License-Identifier: AGPL-3.0-or-later
//! The vsock load's judge against a real curl on loopback, no VM: the bar passes on a healthy run and
//! fails on a stream that is deliberately stalled. Without the second, a green nightly would only
//! mean the detector never fires (the VM tests `vm_perf_vsock_*` use the same script and judge).
#![cfg(unix)]
#![expect(clippy::expect_used, reason = "test: a failed step fails the test")]

use std::time::Instant;

use puddle_vm_tests::BulkHost;
use puddle_vm_tests::load::{Report, Route, parse_transfers, script};

async fn run(url: &str, count: usize, bytes: u64) -> (Report, f64) {
    let started = Instant::now();
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
    let transfers = parse_transfers(&text).expect("curl's lines parse");
    (
        Report::new(count, bytes, transfers),
        started.elapsed().as_secs_f64(),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_healthy_256_way_download_passes_the_bar_and_all_of_it_was_open_together() {
    const BYTES: u64 = 256 * 1024;
    let host = BulkHost::start(256).await.expect("server");
    let url = format!("http://{}/gate/{BYTES}", host.addr());
    let (report, wall) = run(&url, 256, BYTES).await;
    eprintln!("{}", report.summary(wall));
    assert_eq!(report.problems(), Vec::<String>::new());
    assert_eq!(report.completed(), 256);
    assert_eq!(report.moved_bytes(), 256 * BYTES);
    // The server's gate opens only when 256 asked, so all of them were open at once.
    puddle_test_kit::eventually_async(
        "the server counting every body",
        std::time::Duration::from_secs(10),
        || async {
            if host.served() == 256 {
                Ok(())
            } else {
                Err(host.served())
            }
        },
    )
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stream_that_stops_after_64_kib_fails_the_bar_as_stalled() {
    const BYTES: u64 = 1024 * 1024;
    let host = BulkHost::start(1).await.expect("server");
    let url = format!("http://{}/stall/{BYTES}", host.addr());
    let (report, wall) = run(&url, 4, BYTES).await;
    eprintln!("{}", report.summary(wall));
    assert_eq!(report.stalled(), 4, "{:?}", report.problems());
    assert_eq!(report.completed(), 0);
    let problems = report.problems();
    assert!(
        problems
            .iter()
            .any(|p| p.contains("4 transfers went 5 s without progress")),
        "{problems:?}"
    );
    // Detected after about the five seconds, not at once and not only at the 60 s backstop.
    assert!((5.0..30.0).contains(&wall), "detected after {wall} s");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_server_that_cannot_be_reached_fails_the_bar_with_a_named_error() {
    // Port 1 on loopback: refused at once, not a stall.
    let (report, _) = run("http://127.0.0.1:1/gate/10", 3, 10).await;
    let problems = report.problems();
    assert_eq!(report.completed(), 0);
    assert_eq!(report.stalled(), 0);
    assert!(
        problems.iter().any(|p| p.contains("a transfer failed")),
        "{problems:?}"
    );
}
