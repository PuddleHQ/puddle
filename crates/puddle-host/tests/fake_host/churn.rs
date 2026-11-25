// SPDX-License-Identifier: AGPL-3.0-or-later
//! Churn: many create, start, stop, delete cycles and many event-stream connects leave the process
//! with the descriptors and threads it had before.

use super::*;

/// How far a count may drift from its baseline once the churn is over.
const DRIFT: usize = 5;

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
        let (fds, threads) = (self.fds > baseline.fds + DRIFT, self.threads > baseline.threads + DRIFT);
        (fds || threads).then(|| format!("{baseline:?} before, {self:?} after"))
    }
}

/// Waits while the process holds more than `baseline` plus [`DRIFT`] (a socket the server closes
/// after the client left, a worker thread winding down, take a moment) and returns what it holds
/// when it is back, or at the end of the wait.
async fn settled_near(baseline: Holdings) -> Holdings {
    let mut now = Holdings::now();
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
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let (before, after) = churn(200, || create_stop_delete(&api)).await;
    assert_eq!(after.drift_from(before), None);
    assert!(volume_names(&rig).await.is_empty(), "a volume is left");
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn five_hundred_event_stream_connects_and_disconnects_leave_the_descriptors_and_threads_where_they_were()
 {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let (before, after) = churn(500, || async {
        drop(api.events().await);
    })
    .await;
    assert_eq!(after.drift_from(before), None);
    host.shutdown().await;
}

/// The check itself: a descriptor leaked in every cycle is reported.
#[tokio::test(flavor = "multi_thread")]
async fn a_cycle_that_leaks_one_descriptor_is_reported() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let (before, after) = churn(20, || async {
        std::mem::forget(std::fs::File::open("/proc/self/status").unwrap());
        drop(api.events().await);
    })
    .await;
    assert!(after.drift_from(before).is_some(), "{before:?} then {after:?}");
    host.shutdown().await;
}
