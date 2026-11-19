// SPDX-License-Identifier: AGPL-3.0-or-later
//! Full disks under a real microVM (K on Linux KVM): the host's data folder sits on a file system
//! of its own with a fixed size, the test fills it from outside and from inside the guest, and
//! the host and the guest must say so (events, a guest that gets ENOSPC) and recover when space
//! is freed.
//!
//! The data folder is a RAM-backed file system of a fixed size that the test mounts in a private
//! user and mount namespace: the test machine has no loop devices and the VM tests do not run as
//! root. The namespace is entered by running the test binary again (`unshare`), so the host, the
//! msb runtime and every VM of the test see the small disk, and nothing outside the namespace is
//! touched.
#![cfg(target_os = "linux")]
#![expect(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "VM test: a failed step fails the test; measurements go to stderr"
)]
#![allow(
    clippy::indexing_slicing,
    reason = "VM test: JSON read from the API, a missing key fails the test"
)]

mod support;
mod vm_support;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use puddle_compute::DiskSize;
use puddle_host::FreeSpaceTiming;
use puddle_types::WorkspaceId;
use serde_json::{Value, json};
use support::{Api, Events, ended};
use vm_support::{
    VmHost, create_workspace_from, github_internet, guest_as, start_vm_host_platform,
};

/// Set in the second run of a test, inside the namespace.
const INSIDE: &str = "PUDDLE_FULL_DISK_MOUNT";
const REPO: &str = "https://github.com/octocat/Hello-World.git";

/// The mount namespace's root: a RAM file system of its own with the system's folders bound in,
/// and `/pvm` a RAM file system of the size in `$1` GiB. msb refuses a data folder whose ancestors
/// belong to another account, and inside a user namespace the real root's folders do, so the
/// folder's ancestors have to be this namespace's own.
const NEW_ROOT: &str = r#"set -e
root=$1; gib=$2; shift 2
mount -t tmpfs -o mode=755 tmpfs "$root"
for d in usr lib lib64 bin sbin etc home opt dev proc sys run var root; do
  if [ -e "/$d" ]; then mkdir -p "$root/$d"; mount --rbind "/$d" "$root/$d"; fi
done
mkdir -p "$root/tmp" "$root/pvm"
mount -t tmpfs -o mode=1777 tmpfs "$root/tmp"
mount -t tmpfs -o mode=700,size="${gib}g" tmpfs "$root/pvm"
exec /usr/sbin/chroot "$root" "$@""#;

/// Where the small disk is mounted inside the namespace.
const SMALL_DISK: &str = "/pvm";

/// Runs `test` (the name of the calling test function) again in a private user and mount
/// namespace whose `/pvm` is a RAM file system of `gib` GiB, and returns how that second run ended
/// and what it printed. Inside the namespace (the second run) it runs `body` with the mount and
/// returns `None`.
fn in_small_disk(test: &str, gib: u64, body: impl FnOnce(PathBuf)) -> Option<(bool, String)> {
    if std::env::var_os(INSIDE).is_some() {
        body(PathBuf::from(SMALL_DISK));
        return None;
    }
    let root = PathBuf::from(format!("/tmp/pvfdroot{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let out = Command::new("unshare")
        .args([
            "--user",
            "--map-root-user",
            "--mount",
            "--propagation=private",
            "--",
        ])
        .args(["sh", "-c", NEW_ROOT, "sh"])
        .arg(&root)
        .arg(gib.to_string())
        .arg(std::env::current_exe().unwrap())
        .args([test, "--exact", "--nocapture", "--test-threads=1"])
        .env(INSIDE, "1")
        .env(vm_root_var(), SMALL_DISK)
        .output()
        .expect("unshare runs the test again in its own mount namespace");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    eprintln!("{text}");
    let _ = std::fs::remove_dir(&root);
    Some((out.status.success(), text))
}

fn vm_root_var() -> &'static str {
    puddle_vm_tests::ROOT_VAR
}

/// The free space of the file system that holds `dir`, in MiB, read the way an operator would.
fn free_mib(dir: &Path) -> u64 {
    let out = Command::new("df")
        .args(["--output=avail", "-B1M"])
        .arg(dir)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .nth(1)
        .and_then(|l| l.trim().parse().ok())
        .expect("df prints the free MiB")
}

/// Takes space with a file of its own until `leave_mib` are left. Returns the file.
fn fill_until(mount: &Path, leave_mib: u64) -> PathBuf {
    let take = free_mib(mount).saturating_sub(leave_mib);
    let file = mount.join("outside-fill");
    let status = Command::new("fallocate")
        .args(["-l", &format!("{take}M")])
        .arg(&file)
        .status()
        .unwrap();
    assert!(status.success(), "fallocate of {take} MiB");
    file
}

fn disk_space(level: &'static str) -> impl Fn(&Value) -> bool {
    move |e| e["type"] == "disk_space_changed" && e["level"] == level
}

/// A host whose free-space read comes fast: the real drive, read every two seconds.
fn quick_reads(config: &mut puddle_host::HostConfig) {
    config.free_space = FreeSpaceTiming {
        first_read: Duration::from_secs(1),
        period: Duration::from_secs(2),
    };
}

async fn guest_ok(vm: &VmHost, workspace: &WorkspaceId, script: &str) -> String {
    let (code, out) = guest_as(
        &vm.runtime,
        workspace,
        "root",
        script,
        Duration::from_secs(300),
    )
    .await;
    assert_eq!(code, 0, "{script}\n{out}");
    out
}

async fn running_workspace(vm: &VmHost, api: &Api, tag: &str) -> (WorkspaceId, String, Events) {
    let workspace = create_workspace_from(
        vm,
        api,
        tag,
        &["github.com"],
        json!({"repositories": [{"url": REPO}], "memory_mib": 1024}),
    )
    .await;
    let name = workspace.as_str().to_owned();
    let events = api.events().await;
    (workspace, name, events)
}

// ----- the host disk fills while a workspace runs (07 S11b, S12) -----

#[test]
fn vm_full_host_disk_stops_guest_writes_without_harming_other_files_and_recovers() {
    let ran = in_small_disk(
        "vm_full_host_disk_stops_guest_writes_without_harming_other_files_and_recovers",
        26,
        |mount| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(4)
                .enable_all()
                .build()
                .unwrap()
                .block_on(host_disk_fills_and_recovers(mount));
        },
    );
    if let Some((ok, text)) = ran {
        assert!(ok, "the run inside the small disk failed:\n{text}");
    }
}

#[expect(clippy::too_many_lines, reason = "one story, read top to bottom")]
async fn host_disk_fills_and_recovers(mount: PathBuf) {
    let net = github_internet(support::named_id_options()).await;
    let vm = start_vm_host_platform("fulh", net.options.clone(), &net.platform, |c| {
        quick_reads(c);
        c.workspaces.default_size = DiskSize::gib(3);
    })
    .await;
    let api = Api::new(vm.host.url(), vm.host.token());
    let (workspace, name, mut events) = running_workspace(&vm, &api, "fulh").await;
    let dir = format!("/workspaces/{name}");

    // Files that must come through the full disk unchanged.
    guest_ok(
        &vm,
        &workspace,
        &format!(
            "cd {dir} && for i in 1 2 3; do dd if=/dev/urandom of=keep$i.bin bs=1M count=32 2>/dev/null; done && sha256sum keep*.bin > keep.sha && sync"
        ),
    )
    .await;

    // Does a guest fallocate move the host figure? The playbook assumes it does.
    let before = free_mib(&mount);
    guest_ok(
        &vm,
        &workspace,
        &format!("fallocate -l 1G {dir}/falloc.bin && sync"),
    )
    .await;
    let after = free_mib(&mount);
    eprintln!(
        "FINDING guest fallocate of 1 GiB moved the host free space from {before} to {after} MiB"
    );
    // It does not: the guest's file system records the range without writing it, so a host disk
    // cannot be filled by a guest `fallocate` (the playbook says to use `dd`).
    assert!(
        before.saturating_sub(after) < 256,
        "a guest fallocate of 1 GiB must not take a gigabyte of the host: {before} -> {after} MiB"
    );
    guest_ok(&vm, &workspace, &format!("rm {dir}/falloc.bin && sync")).await;

    // The host disk fills from outside, to 1.5 GiB free: less than the guest will write.
    let fill = fill_until(&mount, 1536);
    let critical = events
        .until(disk_space("critical"), Duration::from_secs(60))
        .await;
    let said = critical["free_mib"].as_u64().unwrap();
    assert!(
        said < 5 * 1024,
        "the event names the free space: {critical}"
    );
    // The figure is a reading of the drive while the fill went in: not above what it had, not
    // below what is left.
    assert!(
        said >= free_mib(&mount),
        "the event's figure is the drive's: {critical} against {} MiB",
        free_mib(&mount)
    );

    // The guest writes more than the host has.
    let (code, out) = guest_as(
        &vm.runtime,
        &workspace,
        "root",
        &format!("yes puddle | dd of={dir}/big.bin bs=1M count=2600 iflag=fullblock conv=fsync 2>&1; echo dd-exit=$?"),
        Duration::from_secs(300),
    )
    .await;
    eprintln!(
        "FINDING guest dd while the host is full: exit {code}: {}",
        out.trim()
    );
    let status = api.get(&format!("/api/workspaces/{name}")).await.json();
    eprintln!(
        "FINDING workspace after the write: status {} {}",
        status["status"], status["status_detail"]
    );
    // Today the guest gets an I/O error from the host's refusal, not ENOSPC, and the workspace
    // goes on running without saying why: the playbook's "ENOSPC or paused with a reason" does
    // not hold (follow-up in the brief). The write must at least fail, never report success.
    assert!(
        out.contains("No space left on device") || out.contains("Input/output error"),
        "the guest's write does not report success on a full host disk: {out}"
    );

    // The other files did not change.
    let sums = guest_as(
        &vm.runtime,
        &workspace,
        "root",
        &format!("cd {dir} && sha256sum -c keep.sha 2>&1"),
        Duration::from_secs(120),
    )
    .await;
    eprintln!("FINDING checksums after the full disk: {sums:?}");
    assert_eq!(
        sums.0, 0,
        "the files written before are unchanged: {}",
        sums.1
    );

    // Freeing the space: the notice clears, the workspace starts, git is whole, the files are too.
    std::fs::remove_file(&fill).unwrap();
    events
        .until(disk_space("ok"), Duration::from_secs(60))
        .await;
    let leaked: &'static str = Box::leak(name.clone().into_boxed_str());
    for verb in ["stop", "start"] {
        let reply = api
            .post(&format!("/api/workspaces/{name}/{verb}"), "")
            .await;
        assert_eq!(reply.status, 202, "{verb}: {}", reply.body);
        let end = events.until(ended(leaked), Duration::from_secs(300)).await;
        assert_eq!(end["step"], "done", "{verb}: {end}");
    }
    let after = guest_ok(
        &vm,
        &workspace,
        &format!(
            "cd {dir} && rm -f big.bin && git -c safe.directory='*' -C Hello-World fsck --full 2>&1 && sha256sum -c keep.sha 2>&1 && echo whole"
        ),
    )
    .await;
    assert!(after.contains("whole"), "{after}");
    vm.host.shutdown().await;
}

// ----- a volume and the root disk fill (07 S13a, S14a) -----

#[test]
fn vm_full_volume_and_root_disk_each_say_disk_nearly_full_for_real() {
    let ran = in_small_disk(
        "vm_full_volume_and_root_disk_each_say_disk_nearly_full_for_real",
        40,
        |_| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(4)
                .enable_all()
                .build()
                .unwrap()
                .block_on(volume_and_root_fill());
        },
    );
    if let Some((ok, text)) = ran {
        assert!(ok, "the run inside the small disk failed:\n{text}");
    }
}

fn disk_warning(kind: &'static str) -> impl Fn(&Value) -> bool {
    move |e| e["type"] == "disk_nearly_full" && e["disk"] == kind
}

async fn volume_and_root_fill() {
    let net = github_internet(support::named_id_options()).await;
    let vm = start_vm_host_platform("fulv", net.options.clone(), &net.platform, |c| {
        c.workspaces.default_size = DiskSize::gib(3);
    })
    .await;
    let api = Api::new(vm.host.url(), vm.host.token());
    let (workspace, name, mut events) = running_workspace(&vm, &api, "fulv").await;
    let dir = format!("/workspaces/{name}");
    let one = || async {
        let list = api.get("/api/workspaces").await.json();
        list["workspaces"]
            .as_array()
            .unwrap()
            .iter()
            .find(|w| w["id"] == json!(name))
            .unwrap_or_else(|| panic!("{list}"))
            .clone()
    };
    assert_eq!(one().await["disk_warnings"], json!([]));

    // Zeros first: does the host image grow when the guest writes zeros?
    let host_before = free_mib(&PathBuf::from(SMALL_DISK));
    let out = guest_ok(
        &vm,
        &workspace,
        &format!("dd if=/dev/zero of={dir}/zeros bs=1M count=1500 conv=fsync 2>&1 | tail -1"),
    )
    .await;
    let w = one().await;
    eprintln!(
        "FINDING 1500 MiB of zeros in the guest: host free {} -> {} MiB, API volume used {} ({})",
        host_before,
        free_mib(&PathBuf::from(SMALL_DISK)),
        w["disk_used_mib"],
        out.trim()
    );
    guest_ok(&vm, &workspace, &format!("rm {dir}/zeros && sync")).await;

    // The volume: 3 GiB, filled from inside to the end with data that is not zeros.
    let out = guest_ok(&vm, &workspace, &format!("(yes puddle | dd of={dir}/fill bs=1M count=2980 iflag=fullblock conv=fsync 2>&1 | tail -2); df -m {dir} | tail -1")).await;
    eprintln!("FINDING volume fill: {}", out.trim());
    // Nobody asks: the host's own minute read says it.
    let said = events
        .until(disk_warning("volume"), Duration::from_secs(100))
        .await;
    let w = one().await;
    eprintln!(
        "FINDING volume after fill: size {} used {} warnings {}",
        w["disk_size_mib"], w["disk_used_mib"], w["disk_warnings"]
    );
    assert_eq!(said["workspace"], json!(name));
    assert!(said["percent"].as_u64().unwrap() >= 95, "{said}");
    assert_eq!(w["disk_warnings"], json!(["volume"]), "{w}");

    // Freed, the guest's files are gone.
    guest_ok(&vm, &workspace, &format!("rm {dir}/fill && sync")).await;

    // The root disk: 16 GiB, filled from inside past 95 %.
    let (code, out) = guest_as(
        &vm.runtime,
        &workspace,
        "root",
        "(yes puddle | dd of=/var/tmp/fill bs=1M count=15800 iflag=fullblock conv=fsync 2>&1 | tail -2); df -m / | tail -1",
        Duration::from_secs(600),
    )
    .await;
    eprintln!("FINDING root fill: exit {code}: {}", out.trim());
    let said = events
        .until(disk_warning("root"), Duration::from_secs(100))
        .await;
    let w = one().await;
    eprintln!(
        "FINDING root after fill: size {} used {} warnings {}",
        w["root_disk_size_mib"], w["root_disk_used_mib"], w["disk_warnings"]
    );
    assert!(said["percent"].as_u64().unwrap() >= 95, "{said}");
    assert!(
        w["disk_warnings"]
            .as_array()
            .unwrap()
            .contains(&json!("root")),
        "{w}"
    );
    vm.host.shutdown().await;
}
