// SPDX-License-Identifier: AGPL-3.0-or-later
//! A real guest out-of-memory kill on a real microVM (K on Linux KVM, W on Windows WHP): two
//! workspaces, one with a 1 GiB memory override, and a memory hog running as `agent` in it. The
//! guest kernel's OOM killer ends the hog, and the guest agent reports it through the host to
//! `/api/events` as an `oom_kill` naming that workspace, the process and its pid. The workspace
//! itself keeps running with the memory it was given, and the other workspace's long job is not
//! touched.
#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "VM test: a failed step fails the test"
)]

mod support;
mod vm_support;

use std::sync::Arc;
use std::time::{Duration, Instant};

use puddle_compute::{ExecRequest, Runtime, Sandbox};
use puddle_compute_msb::MsbRuntime;
use puddle_test_kit::eventually_async;
use puddle_types::WorkspaceId;
use serde_json::json;
use support::Api;
use vm_support::{VmHost, create_workspace_from, guest_as, start_vm_host};

/// The memory override of the workspace that gets the hog.
const SMALL_MIB: u64 = 1024;
/// The other workspace's memory: larger, so a mix-up of the two shows in `MemTotal`.
const LARGE_MIB: u64 = 1536;
/// How long after the hog ended its kill must be on the event stream.
const REPORT_WITHIN: Duration = Duration::from_secs(5);

type Handle = Arc<<MsbRuntime as Runtime>::Sandbox>;

/// `MemTotal` of the guest of `workspace`, in KiB.
async fn mem_total_kib(vm: &VmHost, workspace: &WorkspaceId) -> u64 {
    let (code, out) = guest_as(
        &vm.runtime,
        workspace,
        "agent",
        "awk '/^MemTotal:/ {print $2}' /proc/meminfo",
        Duration::from_secs(60),
    )
    .await;
    assert_eq!(code, 0, "{out}");
    out.trim()
        .parse()
        .unwrap_or_else(|_| panic!("MemTotal: {out:?}"))
}

/// The last number the long job wrote, if it has written one.
async fn beat(handle: &Handle) -> Option<u32> {
    let out = handle
        .exec(
            ExecRequest::sh("cat /tmp/beat")
                .as_user("agent")
                .with_timeout(Duration::from_secs(30)),
        )
        .await
        .unwrap();
    out.stdout_text().trim().parse().ok()
}

#[expect(clippy::too_many_lines, reason = "one story, read top to bottom")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vm_oom_kill_in_one_workspace_is_reported_by_name_and_hurts_nothing_else() {
    let vm = start_vm_host("oom").await;
    let api = Api::new(vm.host.url(), vm.host.token());
    let small =
        create_workspace_from(&vm, &api, "small", &[], json!({"memory_mib": SMALL_MIB})).await;
    let large =
        create_workspace_from(&vm, &api, "large", &[], json!({"memory_mib": LARGE_MIB})).await;

    let small_total = mem_total_kib(&vm, &small).await;
    let large_total = mem_total_kib(&vm, &large).await;
    // The kernel keeps some of the RAM for itself, so the guest sees a little less than the
    // override, and the two overrides stay far enough apart to tell them apart.
    assert!(
        (SMALL_MIB * 1024 * 85 / 100..=SMALL_MIB * 1024).contains(&small_total),
        "MemTotal {small_total} KiB is not the {SMALL_MIB} MiB override"
    );
    assert!(
        large_total > small_total + 256 * 1024,
        "{large_total} vs {small_total}"
    );

    // The other workspace's long job, started before the kill and still running after it.
    let handle: Handle = Arc::new(vm.runtime.get(&large.sandbox_name()).await.unwrap());
    let job = {
        let handle = Arc::clone(&handle);
        tokio::spawn(async move {
            handle
                .exec(
                    ExecRequest::sh(
                        "i=0; while [ $i -lt 45 ]; do i=$((i+1)); echo $i > /tmp/beat; sleep 1; done; echo finished",
                    )
                    .as_user("agent")
                    .with_timeout(Duration::from_mins(5)),
                )
                .await
        })
    };
    eventually_async(
        "the long job's first beat",
        Duration::from_secs(30),
        || async { beat(&handle).await.ok_or("no beat yet") },
    )
    .await;

    // The hog: `tail /dev/zero` reads an endless line into memory until the kernel kills it.
    let mut events = api.events().await;
    let (code, said) = guest_as(
        &vm.runtime,
        &small,
        "agent",
        "tail /dev/zero >/dev/null & pid=$!; echo pid=$pid; wait $pid; echo exit=$?",
        Duration::from_mins(3),
    )
    .await;
    let hog_ended = Instant::now();
    assert_eq!(code, 0, "{said}");
    let pid: u32 = said
        .lines()
        .find_map(|l| l.strip_prefix("pid="))
        .and_then(|p| p.parse().ok())
        .unwrap_or_else(|| panic!("no pid in {said:?}"));
    assert!(
        said.contains("exit=137"),
        "the guest did not SIGKILL the hog: {said}"
    );

    let small_name = small.as_str().to_owned();
    let event = events
        .until(
            |e| e["type"] == "oom_kill" && e["workspace"] == small_name.as_str(),
            REPORT_WITHIN,
        )
        .await;
    eprintln!(
        "oom_kill event {:?} after the hog ended: {event}",
        hog_ended.elapsed()
    );
    assert_eq!(event["pid"], pid, "{event}");
    assert_eq!(event["process"], "tail", "{event}");
    let kills: Vec<_> = events
        .seen
        .iter()
        .filter(|e| e["type"] == "oom_kill")
        .collect();
    assert_eq!(
        kills.len(),
        1,
        "one kill, one event, one workspace: {kills:?}"
    );

    // The killed workspace still runs, with the memory it was given.
    let ws = api
        .get(&format!("/api/workspaces/{small_name}"))
        .await
        .json();
    assert_eq!(ws["status"], "running", "{ws}");
    assert_eq!(mem_total_kib(&vm, &small).await, small_total);

    // The other workspace's job went on through it and finishes.
    assert!(
        !job.is_finished(),
        "the other workspace's job ended with the kill"
    );
    let before = beat(&handle).await.unwrap();
    eventually_async(
        "the job to keep beating",
        Duration::from_secs(20),
        || async { (beat(&handle).await.unwrap() > before).then_some(()) },
    )
    .await;
    let done = job.await.unwrap().unwrap();
    assert_eq!(done.status.code, 0);
    assert!(
        done.stdout_text().contains("finished"),
        "{}",
        done.stdout_text()
    );

    let down = vm.host.shutdown().await;
    assert!(down.sandboxes.all_stopped(), "{down:?}");
}
