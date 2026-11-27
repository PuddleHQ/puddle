// SPDX-License-Identifier: AGPL-3.0-or-later
//! A fake of a portable computer's sleep and wake on a real microVM (K on Linux KVM, W on Windows WHP): the
//! VM's process is frozen for five minutes and the route to the network is cut for the same
//! stretch, then both come back, the way they do when a machine wakes. A real suspend is not done
//! (a machine nobody minds is needed for that), so what a real sleep adds (the host's own
//! processes stopping, its monotonic clock standing still, the power states) is not covered;
//! the brief of the test names each of them.
//!
//! The bars are those of a real sleep: one heartbeat gap about as long as the sleep, the
//! workspace still running, the same boot, the guest's clock right within a minute, the route
//! failing with a named message while it is down and working once it is back, an approval that
//! was open before the sleep still open and decidable after it, a new request still asking, and
//! a time-limited grant that ran out during the sleep refused (it is judged by the host's wall
//! clock, which moved the whole sleep) while one that outlasts it still works.
//!
//! Named `vm_soak_` so the per-pull-request jobs skip it (it adds five minutes); the nightly run
//! and a manual dispatch of the VM workflows run it.
#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "VM test: a failed step fails the test; what the guest said goes to the log"
)]

use puddle_host_test_support::support;
use puddle_host_test_support::vm_support;

use std::time::Duration;

use puddle_e2e::lab::WebOptions;
use puddle_store::{Actor, Effect, NewRule, Pattern, Resolution, Scope};
use puddle_test_kit::{eventually, eventually_async};
use puddle_types::{PendingId, WorkspaceId};
use puddle_vm_tests::{FrozenProcess, Gaps, VmEnv, parse_ticks};
use serde_json::json;
use support::{Api, named_id_options};
use vm_support::{
    VmHost, create_workspace_from, field, guest, start_vm_host_platform, web_internet,
};

/// Always allowed.
const STEADY: &str = "steady.invalid";
/// Allowed until the middle of the sleep.
const SHORT_GRANT: &str = "short.invalid";
/// Allowed for an hour: outlasts the sleep.
const LONG_GRANT: &str = "long.invalid";
/// Nothing allows it: the workspace asks.
const ASKED: &str = "asked.invalid";

/// How long the machine sleeps. The length of the hands-on sleep check; a shorter one may be set
/// for a try-out (never under a minute: the gap must be longer than any keep-alive).
fn pause() -> Duration {
    std::env::var("PUDDLE_VM_SLEEP_SECS")
        .ok()
        .and_then(|secs| secs.parse().ok())
        .map_or(Duration::from_mins(5), |secs: u64| {
            Duration::from_secs(secs.max(60))
        })
}

fn host_ms() -> u64 {
    u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
}

fn host_epoch() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64()
}

/// What `curl` through the workspace's proxy settings to `https://site/` did: its words, the HTTP
/// status (`000` when none came), and the exit code.
async fn fetch(vm: &VmHost, workspace: &WorkspaceId, site: &str) -> String {
    guest(
        &vm.runtime,
        workspace,
        &format!(
            "curl -sS -o /dev/null --max-time 30 -w 'http=%{{http_code}}\\n' https://{site}/ 2>&1 </dev/null; echo \"exit=$?\"; true"
        ),
    )
    .await
}

fn reached(out: &str) -> bool {
    field(out, "exit=") == "0" && field(out, "http=") == "200"
}

async fn guest_ticks(vm: &VmHost, workspace: &WorkspaceId) -> Vec<i64> {
    parse_ticks(&guest(&vm.runtime, workspace, "cat /root/hb; true").await)
}

async fn guest_number(vm: &VmHost, workspace: &WorkspaceId, script: &str) -> f64 {
    let out = guest(&vm.runtime, workspace, script).await;
    out.trim()
        .parse()
        .unwrap_or_else(|_| panic!("a number from {script}: {out:?}"))
}

fn open_row(vm: &VmHost, workspace: &WorkspaceId, host: &str) -> Option<PendingId> {
    vm.host
        .store()
        .open_pending_every_user(&puddle_store::testing::all_users(), Some(workspace))
        .unwrap()
        .iter()
        .find(|row| row.host.to_string() == host)
        .map(|row| row.id)
}

async fn wait_for_row(vm: &VmHost, workspace: &WorkspaceId, host: &str) -> PendingId {
    eventually(
        &format!("an open row for {host}"),
        Duration::from_secs(20),
        || open_row(vm, workspace, host).ok_or("no row yet"),
    )
    .await
}

fn grant(vm: &VmHost, workspace: &WorkspaceId, host: &str, expires_at: u64) {
    vm.host
        .store()
        .as_user(puddle_store::UserId::OWNER)
        .add_rule(&NewRule {
            scope: Scope::Workspace(workspace.clone()),
            pattern: Pattern::parse(host).unwrap(),
            effect: Effect::Allow,
            expires_at: Some(expires_at),
            created_by: Actor::Cli,
        })
        .unwrap();
}

#[expect(clippy::too_many_lines, reason = "one story, read top to bottom")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vm_soak_host_sleep_fake_5_min() {
    let pause = pause();
    let net = web_internet(
        &[STEADY, SHORT_GRANT, LONG_GRANT, ASKED],
        WebOptions::default(),
        named_id_options(),
    )
    .await;
    let vm = start_vm_host_platform("sleep", net.options.clone(), &net.platform, |_| {}).await;
    let api = Api::new(vm.host.url(), vm.host.token());
    let workspace =
        create_workspace_from(&vm, &api, "sleep", &[STEADY], json!({"memory_mib": 1024})).await;
    let env = VmEnv::new(vm.settings.clone()).await.unwrap();

    // Before the sleep: three hosts work, one asks and its approval stays open, a heartbeat runs.
    grant(
        &vm,
        &workspace,
        SHORT_GRANT,
        host_ms() + 20_000 + u64::try_from(pause.as_millis() / 2).unwrap(),
    );
    grant(&vm, &workspace, LONG_GRANT, host_ms() + 3_600_000);
    for site in [STEADY, SHORT_GRANT, LONG_GRANT] {
        let out = fetch(&vm, &workspace, site).await;
        assert!(reached(&out), "{site} before the sleep: {out}");
    }
    let refused = fetch(&vm, &workspace, ASKED).await;
    assert!(!reached(&refused), "{refused}");
    let asked = wait_for_row(&vm, &workspace, ASKED).await;
    let started = guest(
        &vm.runtime,
        &workspace,
        "rm -f /root/hb; setsid sh -c 'while :; do date +%s >> /root/hb; sleep 1; done' \
         </dev/null >/dev/null 2>&1 & cat /proc/sys/kernel/random/boot_id",
    )
    .await;
    let boot_id = started.trim().to_owned();
    eventually_async(
        "the guest heartbeat has ticked three times",
        Duration::from_secs(60),
        || async { guest_ticks(&vm, &workspace).await.len() >= 3 },
    )
    .await;
    let uptime_before = guest_number(&vm, &workspace, "cut -d' ' -f1 /proc/uptime").await;

    // The sleep: the route goes first (its sockets die), then the VM stops getting any CPU, and
    // the host's wall clock runs on for the whole stretch.
    let measured_from = std::time::Instant::now();
    let host_before = host_ms() / 1000;
    net.proxy.set_outage(true);
    let pid = env.process_id_of(workspace.as_str()).await.unwrap();
    let mut frozen = FrozenProcess::freeze(pid).unwrap();
    // flake-ok: the sleep is the thing under test: a fixed stretch of host time with the VM stopped
    tokio::time::sleep(pause).await;

    // The wake: the VM runs first and the route is still down, as a machine's network is for the
    // first seconds. A request fails with a message that names the cause, and says so at once.
    frozen.thaw().unwrap();
    let host_after = host_ms() / 1000;
    let paused = i64::try_from(host_after - host_before).unwrap();
    let down = fetch(&vm, &workspace, STEADY).await;
    eprintln!("route down: {down}");
    assert!(!reached(&down), "{down}");
    assert!(
        down.contains("502") || down.contains("CONNECT"),
        "the failure says nothing about the route: {down}"
    );
    net.proxy.set_outage(false);
    eventually_async(
        "the route works again with no restart",
        Duration::from_secs(60),
        || async { reached(&fetch(&vm, &workspace, STEADY).await) },
    )
    .await;

    // The workspace is the one it was.
    eventually_async(
        "the guest heartbeat ticks again after the wake",
        Duration::from_secs(60),
        || async {
            guest_ticks(&vm, &workspace)
                .await
                .last()
                .is_some_and(|tick| i64::try_from(host_after).unwrap() <= *tick)
        },
    )
    .await;
    let gaps = Gaps::of(&guest_ticks(&vm, &workspace).await);
    eprintln!("heartbeat after {paused} s asleep: {gaps:?}");
    assert!(
        gaps.match_pause(paused),
        "one gap about as long as the sleep, saw {gaps:?} for {paused} s"
    );
    let state = api
        .get(&format!("/api/workspaces/{}", workspace.as_str()))
        .await
        .json();
    assert_eq!(state["status"], "running", "{state}");
    let boot_after = guest(
        &vm.runtime,
        &workspace,
        "cat /proc/sys/kernel/random/boot_id; true",
    )
    .await;
    assert_eq!(boot_after.trim(), boot_id, "the guest rebooted");
    eventually_async(
        "the guest clock is within 2 s of the host's",
        Duration::from_secs(60),
        || async {
            let drift = guest_number(&vm, &workspace, "date +%s.%N").await - host_epoch();
            (-2.0..=2.0).contains(&drift).then_some(drift)
        },
    )
    .await;
    let uptime_after = guest_number(&vm, &workspace, "cut -d' ' -f1 /proc/uptime").await;
    eprintln!(
        "REPORT asleep {paused} s; guest uptime minus host time since the sleep began {:.1} s",
        (uptime_after - uptime_before) - measured_from.elapsed().as_secs_f64()
    );

    // Grants are judged by the wall clock: the short one ran out during the sleep, the long one
    // did not.
    let ran_out = fetch(&vm, &workspace, SHORT_GRANT).await;
    assert!(!reached(&ran_out), "the grant outlived its time: {ran_out}");
    let lasting = fetch(&vm, &workspace, LONG_GRANT).await;
    assert!(reached(&lasting), "the long grant ended early: {lasting}");

    // Approvals: the one that was open is still open and decides the host it names; a request
    // made after the wake asks as before.
    assert_eq!(open_row(&vm, &workspace, ASKED), Some(asked));
    wait_for_row(&vm, &workspace, SHORT_GRANT).await;
    vm.host
        .store()
        .resolve_pending(asked, &Resolution::allow(), Actor::Cli)
        .unwrap();
    let decided = fetch(&vm, &workspace, ASKED).await;
    assert!(
        reached(&decided),
        "the approval made before the sleep: {decided}"
    );
    assert!(open_row(&vm, &workspace, ASKED).is_none());

    vm.host.shutdown().await;
}
