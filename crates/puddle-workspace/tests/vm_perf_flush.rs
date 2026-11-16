// SPDX-License-Identifier: AGPL-3.0-or-later
//! The Windows flush path of the host power-loss trial (tier W): a guest `fsync` on a workspace
//! volume makes msb call `FlushFileBuffers` on the volume's disk image.
//!
//! The power-loss trial (`docs/research/2026-11-host-power-loss-trial.md`) cut the power of a Linux
//! host and checked what survived, and an `strace` of the VMM showed guest fsyncs arriving as `fsync`
//! on the image (`poc/power-loss/flushtest.sh`). On Windows there is no power cut here: the
//! question is only whether the same guest fsyncs arrive as flushes of the image file, because the
//! cut semantics (what survives is what was flushed) are the trial's. The trace is ETW
//! (`Microsoft-Windows-Kernel-File`, see `puddle_vm_tests::FlushTrace`), started before the
//! sandbox exists so the trace sees msb open the image.
//!
//! Two windows, told apart by marker files the test creates on the host: 200 guest writes with
//! `O_DSYNC` (each one makes ext4 commit its journal and flush the device), then the same time
//! with the guest idle. The first must show at least one flush per write, all from the VMM
//! process; the second is the control and must show a small fraction of that, so the count is the
//! guest's work and not the background.
//!
//! Named `vm_perf_` so that `ci/vm-filter.sh` runs it on the nightly and on a dispatch only: it
//! needs the administrator rights a hosted runner has, and a trace of the whole runner is not
//! something a merge should wait for.
#![cfg(windows)]
#![expect(clippy::expect_used, reason = "VM test: a failed step fails the test")]

use std::time::Duration;

use microsandbox::Sandbox as SdkSandbox;
use puddle_compute::{DiskSize, ExecRequest, Runtime, Sandbox, SandboxSpec};
use puddle_compute_msb::{MsbConfig, MsbRuntime};
use puddle_types::{ImageRef, MemoryMib, SandboxName, WorkspaceId};
use puddle_vm_tests::{DEBIAN_DEVCONTAINER, FlushTrace, Settings, VmEnv, flushes_between};
use puddle_workspace::{Layout, WorkspaceConfig, Workspaces};

/// Guest writes in the first window.
const WRITES: u64 = 200;
/// How long the control window lasts.
const IDLE: Duration = Duration::from_secs(10);

async fn sh_ok<S: Sandbox>(sb: &S, script: &str) -> String {
    let out = sb
        .exec(
            ExecRequest::sh(script)
                .as_user("root")
                .with_timeout(Duration::from_secs(600)),
        )
        .await
        .expect("exec");
    assert!(
        out.status.success(),
        "{script}: exit {}\n{}{}",
        out.status.code,
        out.stdout_text(),
        out.stderr_text()
    );
    out.stdout_text().trim_end().to_owned()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn vm_perf_guest_fsync_reaches_flushfilebuffers_on_the_volume_image() {
    let settings = Settings::from_lookup(|var| std::env::var(var).ok()).expect("VM test settings");
    let pair = settings.prepare().expect("msb runtime pair");
    let home = settings.home();
    let config = MsbConfig::new(&home, pair.msb, pair.libkrunfw, home.join("guest-share"))
        .with_runtime_log_level(settings.msb_log_level.clone())
        .with_keep_logs_dir(settings.kept_logs());
    let rt = MsbRuntime::open(config).await.expect("open msb");
    let tag = format!("{}-flush", settings.prefix);
    let id = WorkspaceId::new(&tag).unwrap();
    let name = SandboxName::new(&tag).unwrap();
    let mount = Layout::new(&id).unwrap().mount().to_string();
    let image = format!("{}\\disk.raw", id.volume_name().as_str());
    let scratch = tempfile::tempdir().unwrap();
    let work = scratch.path().to_owned();
    let marker = |m: &str| std::fs::write(work.join(format!("flush-mark-{m}-{tag}")), b"").unwrap();

    // The trace starts first: the image's Create event is how it knows which file object is it.
    let trace = FlushTrace::start(&work, &format!("puddle-{tag}")).expect("start the ETW trace");
    let w = Workspaces::new(WorkspaceConfig::default());
    let spec = SandboxSpec::new(name.clone(), ImageRef::new(DEBIAN_DEVCONTAINER).unwrap())
        .with_memory(MemoryMib::new(1024).unwrap());
    let sb = w
        .create(&rt, &id, spec, Some(DiskSize::gib(1)))
        .await
        .unwrap();
    sh_ok(&sb, "sync").await;
    tokio::time::sleep(Duration::from_secs(5)).await;

    marker("a");
    sh_ok(
        &sb,
        &format!("dd if=/dev/zero of={mount}/ft bs=4k count={WRITES} oflag=dsync status=none"),
    )
    .await;
    marker("b");
    tokio::time::sleep(IDLE).await;
    marker("c");

    let pid = {
        let harness = VmEnv::new(settings.clone()).await.unwrap();
        let handle = harness
            .scope(Box::pin(SdkSandbox::get(name.as_str())))
            .await
            .unwrap();
        u32::try_from(handle.local().and_then(|l| l.pid).expect("the VM pid")).unwrap()
    };
    let xml = trace.stop().expect("stop and read the trace");
    eprintln!("trace: {} bytes of XML, VMM pid {pid}", xml.len());

    let writes = flushes_between(
        &xml,
        &image,
        &format!("flush-mark-a-{tag}"),
        &format!("flush-mark-b-{tag}"),
    )
    .unwrap();
    let idle = flushes_between(
        &xml,
        &image,
        &format!("flush-mark-b-{tag}"),
        &format!("flush-mark-c-{tag}"),
    )
    .unwrap();
    eprintln!(
        "{WRITES} guest O_DSYNC writes: {} flushes of the image by process {:?}; idle {} s: {} flushes; event ids {:?}",
        writes.total,
        writes.by_process,
        IDLE.as_secs(),
        idle.total,
        writes.event_ids
    );

    sb.stop().await.unwrap();
    drop(sb);
    rt.remove(&name).await.unwrap();
    rt.remove_volume(&id.volume_name()).await.unwrap();

    assert!(
        writes.total >= WRITES,
        "fewer flushes of {image} than guest writes: {writes:?}"
    );
    assert_eq!(
        writes.by_process.keys().copied().collect::<Vec<_>>(),
        vec![pid],
        "every flush of the image comes from the VMM process"
    );
    assert!(
        idle.total * 10 <= writes.total,
        "the idle control shows {} flushes against {}: the count is not the guest's work",
        idle.total,
        writes.total
    );
}
