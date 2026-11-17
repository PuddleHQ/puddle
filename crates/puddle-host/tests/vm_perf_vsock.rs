// SPDX-License-Identifier: AGPL-3.0-or-later
//! The vsock egress path under load, on a real microVM (K on Linux KVM, W on Windows WHP): 256 parallel
//! downloads from a host-side server go through the guest agent, the vsock, and the host's proxy, and
//! every one must complete with every byte and none may go 5 s without progress (the bar of the
//! original vsock bench). The guest runs one `curl` (`puddle_vm_tests::load`); the server holds every
//! body until all 256 have connected, so the load is simultaneous.
//!
//! The tests are named `vm_perf_` so that `ci/vm-filter.sh` runs them on the nightly and on a
//! dispatch only: a wall-clock bar on a shared runner is too noisy to gate a merge. The run prints
//! its figures (`vsock load: ...`) for the record.
//!
//! `vm_perf_keepalive_request_costs_within_three_times_linux` is the other bar of the same path: the
//! cost of one request on a kept-alive connection (1000 in a row through the proxy), which was 0.52 ms
//! on Linux KVM and 15.5 ms on Windows. On Windows it is marked known-failing, see `KNOWN_FAILING_HERE`.
//!
//! `vm_perf_vsock_detector_fails_on_a_stalled_stream` is the test's check on itself: the same script
//! against a server that stops after 64 KiB must be reported as a stall, in the same VM path. (The
//! detector alone is also tested without a VM, in `puddle-vm-tests/tests/load_selftest.rs`.)
#![expect(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "VM test: a failed step fails the test; figures go to stderr"
)]

mod support;
mod vm_support;

use std::time::{Duration, Instant};

use puddle_compute_msb::MsbRuntime;
use puddle_e2e::lab::WebOptions;
use puddle_store::{Actor, Effect, NewRule, Pattern, Scope};
use puddle_types::WorkspaceId;
use puddle_vm_tests::BulkHost;
use puddle_vm_tests::keepalive;
use puddle_vm_tests::load::{Report, Route, parse_transfers, script};
use serde_json::json;
use support::{Api, ended, named_id_options};
use vm_support::{VmHost, guest_within, start_vm_host_platform, web_internet};

/// The fake site's name: no real name, so nothing leaves the machine.
const BULK_HOST: &str = "bulk.invalid";
/// A site the harness needs only to have a company proxy and a platform.
const FILLER: &str = "filler.invalid";

/// The parallel downloads of the bar.
const CONNECTIONS: usize = 256;
/// Bytes per download: eight times yamux's initial stream window (256 KiB), so every stream needs
/// window updates and one that is not served stalls inside its body, not only at its start.
/// 512 MiB moves in all.
const BYTES: u64 = 2 * 1024 * 1024;
/// A cap on one download, under the guest command's own deadline.
const MAX_TRANSFER_SECS: u32 = 240;
const GUEST_DEADLINE: Duration = Duration::from_secs(300);

/// Requests on one kept-alive connection.
const KEEPALIVE_REQUESTS: usize = 1000;
/// What a kept-alive request cost on Linux KVM, milliseconds (measured: 0.52).
const KEEPALIVE_LINUX_MS: f64 = 0.52;
/// The bar: a request on a kept-alive connection costs at most this many times Linux's figure.
const KEEPALIVE_FACTOR: f64 = 3.0;
/// Windows' kept-alive cost is known to miss the bar (about 15.5 ms a request, hosted and local runs):
/// the test then records the figure and passes while it misses, and fails when it meets the bar, so
/// the fix gets noticed and this marker is deleted with it.
const KNOWN_FAILING_HERE: bool = cfg!(windows);

struct Rig {
    runtime: MsbRuntime,
    sandbox: WorkspaceId,
    // Held for their lifetimes: the host, the fake internet and the server.
    _host: VmHost,
    _bulk: BulkHost,
    _internet: vm_support::WebInternet,
}

/// A running workspace that may reach `bulk.invalid`, which leads to a host-side server whose
/// `/gate/` bodies wait for `gate` askers.
async fn rig(tag: &str, gate: usize) -> Rig {
    let bulk = BulkHost::start(gate).await.expect("the bulk server");
    let internet = web_internet(&[FILLER], WebOptions::default(), named_id_options()).await;
    internet
        .proxy
        .route(&format!("{BULK_HOST}:80"), bulk.addr());
    let host =
        start_vm_host_platform(tag, internet.options.clone(), &internet.platform, |_| {}).await;
    let name = format!("{}-{tag}", host.settings.prefix.as_str());
    let sandbox = WorkspaceId::new(&name).unwrap();
    puddle_store::testing::own(host.host.store(), &sandbox);
    host.host
        .store()
        .as_user(puddle_store::UserId::OWNER)
        .add_rule(&NewRule {
            scope: Scope::Workspace(sandbox.clone()),
            pattern: Pattern::parse(BULK_HOST).unwrap(),
            effect: Effect::Allow,
            expires_at: None,
            created_by: Actor::Cli,
        })
        .unwrap();
    let api = Api::new(host.host.url(), host.host.token());
    let mut events = api.events().await;
    let name_static: &'static str = Box::leak(name.clone().into_boxed_str());
    let reply = api
        .post(
            "/api/workspaces",
            &json!({"name": name, "memory_mib": 1024}).to_string(),
        )
        .await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    let end = events
        .until(ended(name_static), Duration::from_secs(600))
        .await;
    assert_eq!(end["step"], "done", "{end}");
    let runtime = host.runtime.clone();
    Rig {
        runtime,
        sandbox,
        _host: host,
        _bulk: bulk,
        _internet: internet,
    }
}

/// Runs the load in the guest and judges it.
async fn load(rig: &Rig, path: &str, count: usize) -> (Report, f64) {
    let url = format!("http://{BULK_HOST}{path}");
    let started = Instant::now();
    let output = guest_within(
        &rig.runtime,
        &rig.sandbox,
        &script(&url, count, Route::EnvProxyTunnel, MAX_TRANSFER_SECS),
        GUEST_DEADLINE,
    )
    .await;
    let wall = started.elapsed().as_secs_f64();
    let transfers = parse_transfers(&output).expect("curl's lines parse");
    (Report::new(count, BYTES, transfers), wall)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vm_perf_vsock_256_parallel_downloads_complete_without_a_stall() {
    let rig = rig("perf256", CONNECTIONS).await;
    let (report, wall) = load(&rig, &format!("/gate/{BYTES}"), CONNECTIONS).await;
    let load_average = std::fs::read_to_string("/proc/loadavg").unwrap_or_default();
    eprintln!(
        "vsock load: {} (host load average {})",
        report.summary(wall),
        load_average
            .split_whitespace()
            .take(3)
            .collect::<Vec<_>>()
            .join(" ")
    );
    assert_eq!(report.problems(), Vec::<String>::new());
    assert_eq!(report.completed(), CONNECTIONS);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vm_perf_vsock_detector_fails_on_a_stalled_stream() {
    let rig = rig("perfstall", 1).await;
    let (report, wall) = load(&rig, &format!("/stall/{BYTES}"), 8).await;
    eprintln!("vsock stall self-test: {}", report.summary(wall));
    assert_eq!(report.stalled(), 8, "{:?}", report.problems());
    assert_eq!(report.completed(), 0);
    assert!(
        report
            .problems()
            .iter()
            .any(|p| p.contains("went 5 s without progress")),
        "{:?}",
        report.problems()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vm_perf_keepalive_request_costs_within_three_times_linux() {
    let rig = rig("perfkeep", 1).await;
    let url = format!("http://{BULK_HOST}/keepalive/16");
    let output = guest_within(
        &rig.runtime,
        &rig.sandbox,
        &keepalive::script(
            &url,
            KEEPALIVE_REQUESTS,
            Route::EnvProxyTunnel,
            MAX_TRANSFER_SECS,
        ),
        GUEST_DEADLINE,
    )
    .await;
    let report = keepalive::Report::new(
        KEEPALIVE_REQUESTS,
        keepalive::parse_requests(&output).expect("curl's lines parse"),
    );
    eprintln!("keepalive cost: {}", report.summary());
    assert_eq!(report.problems(), Vec::<String>::new());
    let bar = KEEPALIVE_LINUX_MS * KEEPALIVE_FACTOR;
    let cost = report.per_request_ms().expect("1000 requests");
    match (cost <= bar, KNOWN_FAILING_HERE) {
        (true, false) => {}
        (false, true) => eprintln!(
            "keepalive cost: {cost:.3} ms misses the bar of {bar:.2} ms, a known failure here"
        ),
        (false, false) => panic!("a kept-alive request costs {cost:.3} ms, the bar is {bar:.2} ms"),
        (true, true) => panic!(
            "a kept-alive request costs {cost:.3} ms, within the bar of {bar:.2} ms: the known failure is fixed, delete KNOWN_FAILING_HERE"
        ),
    }
}
