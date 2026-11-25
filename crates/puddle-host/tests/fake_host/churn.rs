// SPDX-License-Identifier: AGPL-3.0-or-later
//! Churn: many create, start, stop, delete cycles and many event-stream connects leave the process
//! with the descriptors and threads it had before.

use super::*;

/// How far a count may drift from its baseline once the churn is over.
const DRIFT: usize = 5;

/// Set in the copy of this binary that [`alone`] starts.
const ALONE: &str = "PUDDLE_CHURN_ALONE";

/// Runs `body`, the test `churn::<test>`, with no other test in the process. The counts are the
/// whole process's, and under `cargo test` the other tests of this binary open and close
/// descriptors and threads at the same time (a churn run once counted 74 descriptors of theirs).
/// So unless nextest already gave the test a process of its own, a copy of this binary runs just
/// that test, and its exit is the verdict.
async fn alone<F: Future<Output = ()>>(test: &str, body: impl FnOnce() -> F) {
    let own_process = std::env::var_os(ALONE).is_some()
        || std::env::var("NEXTEST_EXECUTION_MODE").as_deref() == Ok("process-per-test");
    if own_process {
        body().await;
    } else {
        let name = format!("churn::{test}");
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", &name, "--nocapture", "--test-threads=1"])
            .env(ALONE, "1")
            .output()
            .unwrap();
        let report = format!(
            "--- stdout\n{}\n--- stderr\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(out.status.success(), "{name} failed alone\n{report}");
        // An unknown name runs no test and still exits 0.
        assert!(
            report.contains("1 passed"),
            "{name} did not run alone\n{report}"
        );
    }
}

/// What this process holds right now: open descriptors and threads (Linux `/proc/self`).
#[derive(Clone, Copy, Debug)]
struct Holdings {
    fds: usize,
    threads: usize,
}

impl Holdings {
    fn now() -> Self {
        let fds = std::fs::read_dir("/proc/self/fd").unwrap().count();
        let status = std::fs::read_to_string("/proc/self/status").unwrap();
        let threads = status
            .lines()
            .find_map(|l| l.strip_prefix("Threads:"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        Self { fds, threads }
    }

    /// What `self` holds beyond `baseline` plus [`DRIFT`], worded for a failure; `None` when back.
    fn drift_from(self, baseline: Self) -> Option<String> {
        let (fds, threads) = (
            self.fds > baseline.fds + DRIFT,
            self.threads > baseline.threads + DRIFT,
        );
        (fds || threads).then(|| format!("{baseline:?} before, {self:?} after"))
    }
}

/// Waits while the process holds more than `baseline` plus [`DRIFT`] (a socket the server closes
/// after the client left, a worker thread winding down, take a moment) and returns what it holds
/// when it is back, or at the end of the wait.
async fn settled_near(baseline: Holdings) -> Holdings {
    let mut now = Holdings::now();
    // flake-ok: a bounded settle of 10 s whose end is the answer (the report of a leak), which
    // `eventually` would turn into a panic
    for _ in 0..100 {
        if now.drift_from(baseline).is_none() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        now = Holdings::now();
    }
    now
}

/// Runs `cycle` once as a warm-up (lazy set-up is not a leak), takes the baseline, runs it `count`
/// more times and returns the baseline and what the process holds once the cycles are over.
async fn churn<F: Future<Output = ()>>(
    count: usize,
    mut cycle: impl FnMut() -> F,
) -> (Holdings, Holdings) {
    cycle().await;
    let baseline = settled_near(Holdings::now()).await;
    for _ in 0..count {
        cycle().await;
    }
    (baseline, settled_near(baseline).await)
}

async fn create_stop_delete(api: &Api) {
    let mut events = api.events().await;
    let reply = api.post("/api/workspaces", &new_workspace("acme")).await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    let id = reply.json()["id"].as_str().unwrap().to_owned();
    let end = events.until(ended(&id), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "done", "{end}");
    let stopped = api.post(&format!("/api/workspaces/{id}/stop"), "").await;
    assert_eq!(stopped.status, 202, "{}", stopped.body);
    events.until(ended(&id), Duration::from_secs(20)).await;
    let body = json!({"confirm": true}).to_string();
    let gone = api
        .delete(&format!("/api/workspaces/{id}"), Some(&body))
        .await;
    assert_eq!(gone.status, 202, "{}", gone.body);
    let end = events.until(ended(&id), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "done", "{end}");
}

#[tokio::test(flavor = "multi_thread")]
async fn two_hundred_create_start_stop_delete_cycles_leave_the_descriptors_and_threads_where_they_were()
 {
    alone("two_hundred_create_start_stop_delete_cycles_leave_the_descriptors_and_threads_where_they_were", || async {
        let rig = Rig::new();
        let host = rig.start().await;
        let api = api(&host);
        let (before, after) = churn(200, || create_stop_delete(&api)).await;
        assert_eq!(after.drift_from(before), None);
        assert!(volume_names(&rig).await.is_empty(), "a volume is left");
        host.shutdown().await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn five_hundred_event_stream_connects_and_disconnects_leave_the_descriptors_and_threads_where_they_were()
 {
    alone("five_hundred_event_stream_connects_and_disconnects_leave_the_descriptors_and_threads_where_they_were", || async {
        let rig = Rig::new();
        let host = rig.start().await;
        let api = api(&host);
        let (before, after) = churn(500, || async {
            drop(api.events().await);
        })
        .await;
        assert_eq!(after.drift_from(before), None);
        host.shutdown().await;
    })
    .await;
}

/// The check itself: a descriptor leaked in every cycle is reported.
#[tokio::test(flavor = "multi_thread")]
async fn a_cycle_that_leaks_one_descriptor_is_reported() {
    alone("a_cycle_that_leaks_one_descriptor_is_reported", || async {
        let rig = Rig::new();
        let host = rig.start().await;
        let api = api(&host);
        let (before, after) = churn(20, || async {
            Box::leak(Box::new(std::fs::File::open("/proc/self/status").unwrap()));
            drop(api.events().await);
        })
        .await;
        assert!(
            after.drift_from(before).is_some(),
            "{before:?} then {after:?}"
        );
        host.shutdown().await;
    })
    .await;
}
