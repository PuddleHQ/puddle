// SPDX-License-Identifier: AGPL-3.0-or-later
//! Churn on real microVMs (K on Linux KVM), two soak bars:
//!
//! - `vm_soak_churn_five_creates_and_deletes_leave_no_volume_no_vm_process_and_the_disk_back`: a
//!   workspace is made, stopped and deleted five times. After each delete no folder of the
//!   workspace is left under the runtime's home, no process names its sandbox, the runtime lists no
//!   volume of it, and the drive's use is back within 1 % of the drive's size.
//! - `vm_soak_backup_during_a_build_and_a_push_round_passes_and_both_finish`: `puddle backup` (the
//!   API call the CLI makes) while a build and a round of pushes run in the guest; the backup is a
//!   gzip archive and the build and the pushes end with exit 0.
//!
//! The editor's ten opens and closes are in `vm_browser_editor`, where the editor's stand-in shell is.
//!
//! Named `vm_soak_` so the per-pull-request jobs skip them; the nightly run and a manual dispatch
//! of the VM workflows run them.
//!
//! Breakages, each of which must turn the first test red (`PUDDLE_VM_SOAK_BREAK`):
//! `leave-volume-dir` (a folder named for the workspace is made under the runtime's home after the
//! delete: the leftover-folder bar), `leak-disk` (a file of 3 % of the drive is written after the
//! delete: the disk bar). The disk judge itself is unit-tested in `puddle_vm_tests::soak`.
#![cfg(target_os = "linux")]
#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "VM test: a failed step fails the test; measurements go to stderr"
)]
#![expect(
    clippy::indexing_slicing,
    reason = "VM test: JSON read from the API, a missing key fails the test"
)]

mod support;
mod vm_support;

use std::path::{Path, PathBuf};
use std::time::Duration;

use puddle_compute::Runtime as _;
use puddle_secrets::{HostName, Secret, SecretStore, SourceSpec, StoredId, TokenScope};
use puddle_store::{Author, Coverage, CredentialBinding, IdentityDraft};
use puddle_types::WorkspaceId;
use puddle_vm_tests::VmEnv;
use puddle_vm_tests::soak::{DISK_BACK_PERCENT, judge_disk_back};
use serde_json::json;
use support::{Api, ended};
use vm_support::{VmHost, guest_within, start_vm_host, start_vm_host_github};

const ROUNDS: usize = 5;

fn broken(name: &str) -> bool {
    std::env::var("PUDDLE_VM_SOAK_BREAK").is_ok_and(|v| v == name)
}

/// The drive of `path`: bytes used and its size, as `df` reads them.
fn drive_of(path: &Path) -> (u64, u64) {
    let out = std::process::Command::new("df")
        .args(["-B1", "--output=used,size"])
        .arg(path)
        .output()
        .expect("df runs");
    let text = String::from_utf8(out.stdout).unwrap();
    let mut fields = text
        .lines()
        .nth(1)
        .expect("df prints a row")
        .split_whitespace();
    let used = fields.next().unwrap().parse().unwrap();
    let size = fields.next().unwrap().parse().unwrap();
    (used, size)
}

/// Folders below `root` whose name holds `needle`, to a depth that reaches the runtime's volumes.
fn folders_naming(root: &Path, needle: &str, depth: usize) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return found;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if entry.file_name().to_string_lossy().contains(needle) {
            found.push(path.clone());
        }
        if depth > 0 && !path.is_symlink() {
            found.extend(folders_naming(&path, needle, depth - 1));
        }
    }
    found
}

/// Processes of this machine whose command line holds `needle` (msb's runner of a sandbox is named
/// for it), not counting this test's own.
fn processes_naming(needle: &str) -> Vec<u32> {
    let own = std::process::id();
    std::fs::read_dir("/proc")
        .unwrap()
        .flatten()
        .filter_map(|entry| entry.file_name().to_str()?.parse::<u32>().ok())
        .filter(|pid| *pid != own)
        .filter(|pid| {
            std::fs::read(format!("/proc/{pid}/cmdline"))
                .is_ok_and(|raw| String::from_utf8_lossy(&raw).contains(needle))
        })
        .collect()
}

/// Makes a workspace with no repositories and returns its id.
async fn create(api: &Api, name: &str) -> String {
    let mut events = api.events().await;
    let reply = api
        .post(
            "/api/workspaces",
            &json!({"name": name, "repositories": [], "memory_mib": 2048}).to_string(),
        )
        .await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    let id = reply.json()["id"].as_str().unwrap().to_owned();
    let end = events.until(ended(&id), Duration::from_secs(600)).await;
    assert_eq!(end["step"], "done", "{end}");
    id
}

async fn stop_and_delete(api: &Api, id: &str) {
    let mut events = api.events().await;
    let stop = api.post(&format!("/api/workspaces/{id}/stop"), "").await;
    assert_eq!(stop.status, 202, "{}", stop.body);
    events.until(ended(id), Duration::from_secs(300)).await;
    let body = json!({"confirm": true}).to_string();
    let gone = api
        .delete(&format!("/api/workspaces/{id}"), Some(&body))
        .await;
    assert_eq!(gone.status, 202, "{}", gone.body);
    let end = events.until(ended(id), Duration::from_secs(300)).await;
    assert_eq!(end["step"], "done", "{end}");
}

/// One round: make, stop and delete a workspace; returns what is left of it.
async fn round(vm: &VmHost, api: &Api) -> Vec<String> {
    let id = create(api, "soakspace").await;
    let sandbox = WorkspaceId::new(&id).unwrap();
    // The runner's process id, to see it gone after the delete.
    let runner = VmEnv::new(vm.settings.clone())
        .await
        .unwrap()
        .process_id_of(sandbox.as_str())
        .await
        .expect("msb records the VM pid");
    stop_and_delete(api, &id).await;

    if broken("leave-volume-dir") {
        std::fs::create_dir_all(vm.settings.home().join(format!("ws-{id}"))).unwrap();
    }
    if broken("leak-disk") {
        let (_, size) = drive_of(&vm.root);
        // `fallocate` reserves the blocks (a sparse file would use none).
        let made = std::process::Command::new("fallocate")
            .args(["-l", &(size / 100 * 3).to_string()])
            .arg(vm.root.join(format!("leaked-{id}")))
            .status()
            .expect("fallocate runs");
        assert!(made.success(), "fallocate failed: {made}");
    }

    let mut left = Vec::new();
    for dir in folders_naming(&vm.settings.home(), &id, 4) {
        left.push(format!("folder {}", dir.display()));
    }
    let volumes = vm.runtime.list_volumes().await.unwrap();
    for volume in volumes.into_iter().filter(|v| v.name.contains(&id)) {
        left.push(format!("volume {}", volume.name));
    }
    if Path::new(&format!("/proc/{runner}")).exists() {
        left.push(format!("runner process {runner}"));
    }
    for pid in processes_naming(&id) {
        left.push(format!("process {pid} names {id}"));
    }
    left
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vm_soak_churn_five_creates_and_deletes_leave_no_volume_no_vm_process_and_the_disk_back() {
    let vm = start_vm_host("chn").await;
    let api = Api::new(vm.host.url(), vm.host.token());
    // The first round pulls the image and warms the caches: its disk use is the baseline.
    let left = round(&vm, &api).await;
    assert!(left.is_empty(), "round 0 left {left:?}");
    for n in 1..=ROUNDS {
        let (before, size) = drive_of(&vm.root);
        let left = round(&vm, &api).await;
        assert!(left.is_empty(), "round {n} left {left:?}");
        let (after, _) = drive_of(&vm.root);
        eprintln!("round {n}: drive use {before} -> {after} of {size} bytes");
        judge_disk_back(before, after, size)
            .unwrap_or_else(|e| panic!("round {n}: {e} (bar {DISK_BACK_PERCENT} %)"));
    }
    let down = vm.host.shutdown().await;
    assert!(down.sandboxes.all_stopped(), "{down:?}");
}

/// The build: thirty gzip runs over random data, each a few seconds of CPU and a burst of disk.
const BUILD: &str = "set -eu
for i in $(seq 1 30); do head -c 8000000 /dev/urandom | gzip -9 > /tmp/build-$i.gz; done
echo BUILD_OK";

/// The push round: clone, then five commits each pushed at once.
const PUSHES: &str = r#"set -eu
rm -rf /tmp/pushes; mkdir /tmp/pushes; cd /tmp/pushes
git clone -q https://github.com/octocat/Hello-World.git w
cd w
for i in 1 2 3 4 5; do
  echo $i > pushed-$i.txt
  git add pushed-$i.txt
  git -c user.name=soak -c user.email=soak@example.org commit -q -m "round $i"
  git push -q origin HEAD:main
  sleep 2
done
echo PUSHES_OK"#;

/// The token the fake github accepts for a push: it takes no anonymous push, so the workspace's
/// identity holds this one and the proxy adds it.
const PUSH_TOKEN: &str = "KCANARY-SOAK-PUSH-3f9a1c";

fn push_identity(vm: &VmHost) -> IdentityDraft {
    let host = HostName::new("github.com").unwrap();
    let id = StoredId::new("soakpush").unwrap();
    vm.secrets
        .set(&id, &Secret::new(PUSH_TOKEN.to_owned()))
        .unwrap();
    let binding = CredentialBinding::new(
        &host,
        SourceSpec::Stored {
            id,
            scope: TokenScope {
                host: host.clone(),
                org: None,
            },
        },
        Coverage::new(std::collections::BTreeSet::new(), true).unwrap(),
    )
    .unwrap();
    IdentityDraft {
        label: "Soak".to_owned(),
        author: Author::new("Soak", "soak@example.org").unwrap(),
        credentials: vec![binding],
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vm_soak_backup_during_a_build_and_a_push_round_passes_and_both_finish() {
    let (vm, net) = start_vm_host_github("bkp").await;
    net.git.accept_token(PUSH_TOKEN);
    vm.host
        .store()
        .create_identity(puddle_store::UserId::OWNER, push_identity(&vm))
        .unwrap();
    let api = Api::new(vm.host.url(), vm.host.token());
    let sandbox = vm_support::create_workspace_from(
        &vm,
        &api,
        "bkp",
        &["github.com"],
        json!({"repositories": [{"url": "https://github.com/octocat/Hello-World.git", "push": true}]}),
    )
    .await;
    let pushed_before = net
        .git
        .seen()
        .iter()
        .filter(|r| r.target.contains("receive-pack"))
        .count();

    let build = guest_within(&vm.runtime, &sandbox, BUILD, Duration::from_secs(600));
    let pushes = guest_within(&vm.runtime, &sandbox, PUSHES, Duration::from_secs(600));
    let backup = async {
        // Let both start before the archive is taken.
        tokio::time::sleep(Duration::from_secs(5)).await;
        api.post("/api/backup", "").await
    };
    let (build, pushes, backup) = tokio::join!(build, pushes, backup);

    assert_eq!(backup.status, 200, "backup: {}", backup.status);
    // The body is gzip (0x1f 0x8b): the first byte is the one that survives the lossy text.
    assert!(backup.body.starts_with('\u{1f}'), "not an archive");
    assert!(build.contains("BUILD_OK"), "the build did not end: {build}");
    assert!(
        pushes.contains("PUSHES_OK"),
        "the pushes did not end: {pushes}"
    );
    let landed = net
        .git
        .seen()
        .iter()
        .filter(|r| r.target.contains("receive-pack"))
        .count();
    assert!(landed > pushed_before, "no push reached the host");
    let down = vm.host.shutdown().await;
    assert!(down.sandboxes.all_stopped(), "{down:?}");
}
