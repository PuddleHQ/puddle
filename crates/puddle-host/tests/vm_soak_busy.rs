// SPDX-License-Identifier: AGPL-3.0-or-later
//! Busy workspaces on real microVMs (K on Linux KVM, W on Windows WHP), the two soak bars of a
//! day of builds:
//!
//! - `vm_soak_busy_build_keeps_the_heartbeat_stops_in_time_and_resumes`: a CPU- and disk-bound
//!   job runs for 20 minutes beside a one-second heartbeat that goes through the route (one
//!   request to the site the workspace is allowed, one tick per answer). No gap over 5 s and the
//!   workspace stays running; Stop while the job runs ends within 75 s; after Start every file of
//!   the build tree that existed when Stop was pressed is there with the same size and
//!   modification time, and an incremental rebuild redoes next to nothing.
//! - `vm_soak_four_busy_workspaces_and_a_bystander`: four workspaces each run a CPU, an IO and a
//!   network job (a package mirror the rules allow) round after round; all exit 0, a fifth,
//!   idle workspace's API answers stay under a bound, and the host's memory and load are logged.
//!
//! Named `vm_soak_` so the per-pull-request jobs skip them (20 and 5 minutes); the nightly run
//! and a manual dispatch of the VM workflows run them.
//!
//! Breakages, each of which must turn the named bar red (`PUDDLE_VM_SOAK_BREAK`):
//! `route-pause` (the route pauses for 10 s in the middle of the build: the heartbeat bar),
//! `touch-tree` (every file of the tree is touched after Start: the tree and the incremental
//! bar), `tight-stop` (the Stop bar is 2 s), `mirror-500` (the mirror answers 500: the exit
//! codes of the jobs). The bars' own judges are unit-tested in `puddle_vm_tests::soak`.

mod support;
mod vm_support;

use std::sync::Arc;
use std::time::{Duration, Instant};

use puddle_e2e::lab::{Responder, WebAnswer, WebOptions};
use puddle_test_kit::eventually_async;
use puddle_types::WorkspaceId;
use puddle_vm_tests::soak::{
    STOP_WITHIN_SECS, judge_heartbeat, judge_incremental, judge_latency, judge_rounds, parse_tree,
    tree_changes,
};
use puddle_vm_tests::{parse_ticks, resident_bytes};
use serde_json::json;
use support::{Api, ended, named_id_options};
use vm_support::{VmHost, create_workspace_from, guest_as, start_vm_host_platform, web_internet};

/// The site the heartbeat asks, and the one the package mirror answers as.
const STEADY: &str = "steady.invalid";
const MIRROR: &str = "mirror.invalid";
/// Units of the build the incremental rebuild is judged on.
const PLAN_UNITS: u32 = 200;
/// How many of the units that the job adds after the plan (one every 2 s, to keep disk writes
/// going until Stop) there can be.
const LAST_UNIT: u32 = 600;
/// How long past the watched stretch the job's own deadline lies: the Stop must find it running
/// however long the checks after the stretch took.
const JOB_OUTLASTS_SECS: u64 = 600;
/// A package of the mirror: 24 MiB.
const PACKAGE_BYTES: usize = 24 * 1024 * 1024;
/// What the idle workspace's API may take to answer while four others are busy.
const BYSTANDER_BOUND_MS: f64 = 3_000.0;

fn secs_from(var: &str, default: Duration, floor: u64) -> Duration {
    std::env::var(var)
        .ok()
        .and_then(|secs| secs.parse().ok())
        .map_or(default, |secs: u64| Duration::from_secs(secs.max(floor)))
}

fn broken(name: &str) -> bool {
    std::env::var("PUDDLE_VM_SOAK_BREAK").is_ok_and(|v| v == name)
}

/// The build: units `$1..$2` of source (made when missing) and object (made when missing or older
/// than its source), each written under a temporary name and moved into place, so a file in the
/// tree is whole and never written again. Prints `REBUILT <n>`.
const BUILD_SH: &str = r#"set -u
cd "$HOME/soak" || exit 1
n=0
for i in $(seq "$1" "$2"); do
  [ -e src/$i ] || { head -c 2400000 /dev/urandom | base64 > src/$i.tmp && mv src/$i.tmp src/$i; }
  if [ ! -e obj/$i.gz ] || [ src/$i -nt obj/$i.gz ]; then
    gzip -9 < src/$i > obj/$i.tmp && mv obj/$i.tmp obj/$i.gz && n=$((n+1))
  fi
done
echo REBUILT $n
"#;

/// The load until the deadline `$1` (epoch seconds): a CPU burner per core, a writer that fsyncs
/// 256 MiB over and over, and more units of the build, one every 2 s.
const JOB_SH: &str = r#"end=$1
cd "$HOME/soak" || exit 1
for c in $(seq 1 "$(nproc)"); do sha256sum /dev/zero > /dev/null & done
( while :; do dd if=/dev/zero of=spill/x bs=1M count=256 conv=fsync 2>/dev/null; done ) &
i=$((PLAN + 1))
while [ "$(date +%s)" -lt "$end" ]; do
  [ "$i" -le "$LAST" ] && { bash build.sh $i $i > /dev/null; i=$((i+1)); }
  sleep 2
done
kill $(jobs -p)
"#;

/// One tick per answer of the allowed site, a second apart: a route that stops shows as a gap.
const HEARTBEAT_SH: &str = r#"cd "$HOME/soak" || exit 1
while :; do
  curl -sS -o /dev/null --fail --max-time 4 https://steady.invalid/ 2>/dev/null && date +%s >> hb
  sleep 1
done
"#;

async fn agent(vm: &VmHost, workspace: &WorkspaceId, script: &str, within: Duration) -> String {
    let (code, out) = guest_as(&vm.runtime, workspace, "agent", script, within).await;
    assert_eq!(code, 0, "{script}\n{out}");
    out
}

/// Writes the three scripts of the build test into `$HOME/soak` of the workspace.
async fn install_scripts(vm: &VmHost, workspace: &WorkspaceId) {
    let job = JOB_SH;
    agent(
        vm,
        workspace,
        &format!(
            "mkdir -p $HOME/soak/src $HOME/soak/obj $HOME/soak/spill && cd $HOME/soak\n\
             cat > build.sh <<'EOF_BUILD'\n{BUILD_SH}EOF_BUILD\n\
             cat > job.sh <<'EOF_JOB'\nPLAN={PLAN_UNITS}; LAST={LAST_UNIT}\n{job}EOF_JOB\n\
             cat > hb.sh <<'EOF_HB'\n{HEARTBEAT_SH}EOF_HB\n"
        ),
        Duration::from_secs(60),
    )
    .await;
}

/// The tree as the guest lists it: whole files only.
async fn tree(vm: &VmHost, workspace: &WorkspaceId) -> String {
    agent(
        vm,
        workspace,
        "cd $HOME/soak && find src obj -type f ! -name '*.tmp' -printf '%p %T@ %s\\n'",
        Duration::from_secs(60),
    )
    .await
}

async fn ticks(vm: &VmHost, workspace: &WorkspaceId) -> Vec<i64> {
    parse_ticks(
        &agent(
            vm,
            workspace,
            "cat $HOME/soak/hb 2>/dev/null; true",
            Duration::from_secs(60),
        )
        .await,
    )
}

async fn status(api: &Api, workspace: &WorkspaceId) -> String {
    let ws = api
        .get(&format!("/api/workspaces/{}", workspace.as_str()))
        .await
        .json();
    ws.get("status")
        .and_then(|s| s.as_str())
        .unwrap_or_default()
        .to_owned()
}

/// Posts `action` (`stop` or `start`) and waits for the progress event that ends it; returns how
/// long that took.
async fn change(api: &Api, workspace: &WorkspaceId, action: &str) -> Duration {
    let name: &'static str = Box::leak(workspace.as_str().to_owned().into_boxed_str());
    let mut events = api.events().await;
    let from = Instant::now();
    let reply = api
        .post(&format!("/api/workspaces/{name}/{action}"), "")
        .await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    let end = events.until(ended(name), Duration::from_mins(5)).await;
    assert_eq!(
        end.get("step").and_then(|s| s.as_str()),
        Some("done"),
        "{end}"
    );
    from.elapsed()
}

#[expect(clippy::too_many_lines, reason = "one story, read top to bottom")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vm_soak_busy_build_keeps_the_heartbeat_stops_in_time_and_resumes() {
    let busy = secs_from("PUDDLE_VM_SOAK_BUSY_SECS", Duration::from_mins(20), 60);
    let stop_bar = if broken("tight-stop") {
        2
    } else {
        STOP_WITHIN_SECS
    };
    let net = web_internet(&[STEADY], WebOptions::default(), named_id_options()).await;
    let vm = start_vm_host_platform("soakbusy", net.options.clone(), &net.platform, |_| {}).await;
    let api = Api::new(vm.host.url(), vm.host.token());
    let workspace =
        create_workspace_from(&vm, &api, "busy", &[STEADY], json!({"memory_mib": 2048})).await;
    install_scripts(&vm, &workspace).await;

    // The full build, timed: what an incremental rebuild is judged against.
    let from = Instant::now();
    let built = agent(
        &vm,
        &workspace,
        &format!("bash $HOME/soak/build.sh 1 {PLAN_UNITS}"),
        Duration::from_mins(15),
    )
    .await;
    let full = from.elapsed().as_secs_f64();
    assert!(
        built.contains(&format!("REBUILT {PLAN_UNITS}")),
        "a full build makes every unit: {built}"
    );
    let cores = agent(&vm, &workspace, "nproc", Duration::from_secs(30)).await;
    eprintln!(
        "full build of {PLAN_UNITS} units: {full:.1} s on {} core(s)",
        cores.trim()
    );

    // The heartbeat first, then the load.
    agent(
        &vm,
        &workspace,
        "cd $HOME/soak && rm -f hb && setsid nohup bash hb.sh >/dev/null 2>&1 </dev/null & echo up",
        Duration::from_secs(30),
    )
    .await;
    eventually_async(
        "the heartbeat has ticked three times",
        Duration::from_secs(60),
        || async { ticks(&vm, &workspace).await.len() >= 3 },
    )
    .await;
    let end = agent(&vm, &workspace, "date +%s", Duration::from_secs(30))
        .await
        .trim()
        .parse::<u64>()
        .unwrap()
        + busy.as_secs()
        + JOB_OUTLASTS_SECS;
    agent(
        &vm,
        &workspace,
        &format!(
            "cd $HOME/soak && setsid nohup bash job.sh {end} >job.log 2>&1 </dev/null & echo up"
        ),
        Duration::from_secs(30),
    )
    .await;

    // The busy stretch: a look once a minute. The workspace stays running and the job alive.
    let started = Instant::now();
    let mut paused = false;
    // flake-ok: a fixed stretch of load is the thing under test; each minute is checked, not waited for
    while started.elapsed() < busy {
        tokio::time::sleep(Duration::from_secs(60).min(busy.saturating_sub(started.elapsed())))
            .await;
        if broken("route-pause") && !paused && started.elapsed() > busy / 2 {
            paused = true;
            net.proxy.set_outage(true);
            // flake-ok: the breakage is a route that stands still for a fixed 10 s
            tokio::time::sleep(Duration::from_secs(10)).await;
            net.proxy.set_outage(false);
        }
        assert_eq!(status(&api, &workspace).await, "running");
        let alive = agent(
            &vm,
            &workspace,
            "pgrep -fc '[b]ash job.sh' || true",
            Duration::from_secs(60),
        )
        .await;
        eprintln!(
            "busy {} s, job processes {}",
            started.elapsed().as_secs(),
            alive.trim()
        );
    }

    // Bar 1: no gap over 5 s through the whole stretch.
    let all = ticks(&vm, &workspace).await;
    let min_ticks = usize::try_from(busy.as_secs() / 5).unwrap();
    let gaps = judge_heartbeat(&all, min_ticks).unwrap_or_else(|why| panic!("{why}"));
    eprintln!(
        "heartbeat over {} s: {} ticks, {gaps:?}",
        busy.as_secs(),
        all.len()
    );
    assert_eq!(status(&api, &workspace).await, "running");

    // Bar 2: Stop while the job still runs. The tree is read just before: every file in it is
    // whole and never written again, so nothing in the listing may differ afterwards.
    let alive = agent(
        &vm,
        &workspace,
        "pgrep -fc '[b]ash job.sh' || true",
        Duration::from_secs(60),
    )
    .await;
    assert!(
        alive.trim() != "0",
        "the job ended before the Stop: {alive}"
    );
    let before = parse_tree(&tree(&vm, &workspace).await);
    assert!(
        before.len() >= 2 * PLAN_UNITS as usize,
        "the tree before Stop has {} files",
        before.len()
    );
    let stopped_in = change(&api, &workspace, "stop").await;
    eprintln!("Stop on a busy workspace: {stopped_in:?}");
    assert_eq!(status(&api, &workspace).await, "stopped");
    // flake-ok: the bar is the spec's 75 s for a Stop on a busy workspace, a product limit on a VM host, not a margin
    assert!(
        stopped_in < Duration::from_secs(stop_bar),
        "Stop took {stopped_in:?}, bar {stop_bar} s"
    );

    // Bar 3: after Start the tree is as it was and a rebuild is short.
    let started_in = change(&api, &workspace, "start").await;
    eprintln!("Start after the stop: {started_in:?}");
    if broken("touch-tree") {
        agent(
            &vm,
            &workspace,
            "cd $HOME/soak && touch src/* obj/*",
            Duration::from_secs(60),
        )
        .await;
    }
    let after = parse_tree(&tree(&vm, &workspace).await);
    let changes = tree_changes(&before, &after);
    assert!(
        changes.is_empty(),
        "{} of {} files changed or lost, first: {:?}",
        changes.len(),
        before.len(),
        changes.iter().take(5).collect::<Vec<_>>()
    );
    let from = Instant::now();
    let rebuilt = agent(
        &vm,
        &workspace,
        &format!("bash $HOME/soak/build.sh 1 {PLAN_UNITS}"),
        Duration::from_mins(15),
    )
    .await;
    let secs = from.elapsed().as_secs_f64();
    let units: u64 = rebuilt
        .lines()
        .find_map(|l| l.strip_prefix("REBUILT "))
        .and_then(|n| n.trim().parse().ok())
        .unwrap_or_else(|| panic!("no REBUILT line in {rebuilt:?}"));
    eprintln!("incremental rebuild: {units} units in {secs:.1} s (full {full:.1} s)");
    judge_incremental(units, 2, secs, full).unwrap_or_else(|why| panic!("{why}"));

    let down = vm.host.shutdown().await;
    assert!(down.sandboxes.all_stopped(), "{down:?}");
}

/// The mirror: `/pool/<anything>.bin` is a 24 MiB package; with the breakage it answers 500.
fn mirror() -> WebOptions {
    let body = Arc::new(vec![b'p'; PACKAGE_BYTES]);
    let responder: Responder = Arc::new(move |seen| {
        let on_mirror = seen
            .headers_named("host")
            .first()
            .is_some_and(|h| h.split(':').next() == Some(MIRROR));
        if !on_mirror || !seen.target.starts_with("/pool/") {
            return None;
        }
        Some(if broken("mirror-500") {
            WebAnswer {
                status: 500,
                content_type: "text/plain",
                body: b"mirror down".to_vec(),
            }
        } else {
            WebAnswer {
                status: 200,
                content_type: "application/octet-stream",
                body: body.as_ref().clone(),
            }
        })
    });
    WebOptions {
        responder: Some(responder),
        ..WebOptions::default()
    }
}

/// One workspace's rounds: a CPU, an IO and a network job at once, until `$END`. Prints one
/// `ROUND cpu=<exit> io=<exit> net=<exit>` per round.
const ROUNDS_SH: &str = r#"mkdir -p "$HOME/round" && cd "$HOME/round" || exit 1
while [ "$(date +%s)" -lt "$END" ]; do
  ( head -c 200M /dev/urandom | gzip -9 > /dev/null ) & cpu=$!
  ( dd if=/dev/zero of=io.bin bs=1M count=512 conv=fsync 2>/dev/null && sha256sum io.bin > /dev/null && rm -f io.bin ) & io=$!
  ( for k in 1 2 3 4; do
      curl -sS --fail --max-time 120 -o /dev/null -w '%{size_download}\n' "https://mirror.invalid/pool/$k.bin" &
    done | awk '$1 != 25165824 { bad = 1 } END { exit bad || NR != 4 }' ) & net=$!
  wait $cpu; a=$?; wait $io; b=$?; wait $net; c=$?
  echo "ROUND cpu=$a io=$b net=$c"
done
"#;

/// Seconds of `/proc/loadavg`'s first field and the memory the host has available, for the log.
fn host_load() -> String {
    let load = std::fs::read_to_string("/proc/loadavg").unwrap_or_default();
    let mem = std::fs::read_to_string("/proc/meminfo")
        .unwrap_or_default()
        .lines()
        .find(|l| l.starts_with("MemAvailable:"))
        .unwrap_or_default()
        .to_owned();
    format!(
        "load {} / {mem}",
        load.split_whitespace().next().unwrap_or("?")
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vm_soak_four_busy_workspaces_and_a_bystander() {
    let busy = secs_from("PUDDLE_VM_SOAK_MULTI_SECS", Duration::from_mins(5), 30);
    let net = web_internet(&[MIRROR], mirror(), named_id_options()).await;
    let vm = start_vm_host_platform("soakmulti", net.options.clone(), &net.platform, |_| {}).await;
    let api = Api::new(vm.host.url(), vm.host.token());
    let mut workers = Vec::new();
    for n in 1..=4 {
        workers.push(
            create_workspace_from(
                &vm,
                &api,
                &format!("work{n}"),
                &[MIRROR],
                json!({"memory_mib": 1024}),
            )
            .await,
        );
    }
    let bystander =
        create_workspace_from(&vm, &api, "bystander", &[], json!({"memory_mib": 1024})).await;
    eprintln!("five workspaces up; host before the load: {}", host_load());

    let end = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + busy.as_secs();
    let runs: Vec<_> = workers
        .iter()
        .map(|workspace| {
            let (vm_runtime, workspace) = (vm.runtime.clone(), workspace.clone());
            tokio::spawn(async move {
                let (code, out) = guest_as(
                    &vm_runtime,
                    &workspace,
                    "agent",
                    &format!("END={end}\n{ROUNDS_SH}"),
                    busy + Duration::from_mins(10),
                )
                .await;
                (workspace, code, out)
            })
        })
        .collect();

    // The bystander's API, asked twice a second while the others work.
    let mut samples = Vec::new();
    let mut peak_rss = 0_u64;
    let mut last_log = Instant::now();
    // flake-ok: samples the bystander's API while the four jobs run; the loop ends when they do, it waits for no condition
    while !runs.iter().all(tokio::task::JoinHandle::is_finished) {
        let from = Instant::now();
        let ws = api
            .get(&format!("/api/workspaces/{}", bystander.as_str()))
            .await;
        samples.push(from.elapsed().as_secs_f64() * 1000.0);
        assert_eq!(ws.status, 200, "{}", ws.body);
        peak_rss = peak_rss.max(resident_bytes(std::process::id()).unwrap_or(0));
        if last_log.elapsed() > Duration::from_secs(60) {
            last_log = Instant::now();
            eprintln!(
                "host RSS {} MiB; {}",
                resident_bytes(std::process::id()).unwrap_or(0) >> 20,
                host_load()
            );
        }
        // flake-ok: a sampling interval, not a wait for a condition
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    eprintln!("host after the load: {}", host_load());
    eprintln!("REPORT host RSS peak {} MiB", peak_rss >> 20);
    let latency =
        judge_latency(&samples, 10, BYSTANDER_BOUND_MS).unwrap_or_else(|why| panic!("{why}"));
    eprintln!(
        "REPORT bystander API: {} answers, slowest {:.0} ms",
        latency.samples, latency.worst_ms
    );

    for run in runs {
        let (workspace, code, out) = run.await.unwrap();
        assert_eq!(code, 0, "{}: {out}", workspace.as_str());
        let rounds =
            judge_rounds(&out, 1).unwrap_or_else(|why| panic!("{}: {why}", workspace.as_str()));
        eprintln!("{}: {rounds} round(s), all exit 0", workspace.as_str());
    }
    assert_eq!(status(&api, &bystander).await, "running");

    let down = vm.host.shutdown().await;
    assert!(down.sandboxes.all_stopped(), "{down:?}");
}
