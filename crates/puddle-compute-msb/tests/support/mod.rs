// SPDX-License-Identifier: AGPL-3.0-or-later
//! Shared setup of the VM tests: the adapter on the VM harness's private msb home.
#![allow(
    dead_code,
    reason = "each test binary uses a different subset of the helpers"
)]
#![expect(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "a failed setup fails the test"
)]

use std::time::Duration;

use puddle_compute::probes::VSOCK_SWEEP;
use puddle_compute::{ExecOutput, ExecRequest, Runtime, Sandbox, SandboxSpec};
use puddle_compute_msb::{MsbConfig, MsbRuntime};
use puddle_ipc::Listener;
use puddle_types::{GuestPath, ImageRef, MemoryMib, SandboxName};
use puddle_vm_tests::{DEBIAN_DEVCONTAINER, Settings};
use tokio::io::AsyncReadExt;

pub(crate) mod mount_attack;

/// A throwaway public key (its private half was never kept): the SDK's SSH server refuses to
/// start without an authorized key.
pub(crate) const TEST_SSH_KEY: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOrqAVBGG1BD2K0dsi6Z6il4+PlI8R/egmhi8Ox00c0/ puddle-contract-test";

/// The harness settings (runtime pair, run prefix, private home) from the environment.
pub(crate) fn settings() -> Settings {
    Settings::from_lookup(|var| std::env::var(var).ok()).expect("VM test settings")
}

/// The adapter on the run's private msb home, with `<home>/guest-share` as the mount root. Its
/// warnings (a lost boot race, with msb's log tail) go to the test's stderr.
pub(crate) async fn runtime(settings: &Settings) -> MsbRuntime {
    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .try_init();
    let pair = settings.prepare().expect("msb runtime pair");
    let home = settings.home();
    let config = MsbConfig::new(&home, pair.msb, pair.libkrunfw, home.join("guest-share"))
        // The tests keep no data folder of their own; the home stands in for it.
        .with_protected_dir(&home)
        .with_ssh_key(TEST_SSH_KEY)
        .with_runtime_log_level(settings.msb_log_level.clone())
        .with_keep_logs_dir(settings.kept_logs());
    MsbRuntime::open(config).await.expect("open msb")
}

/// A run-prefixed sandbox name.
pub(crate) fn name(settings: &Settings, tag: &str) -> SandboxName {
    SandboxName::new(&format!("{}-{tag}", settings.prefix)).unwrap()
}

/// A guest path that must be valid.
pub(crate) fn path(p: &str) -> GuestPath {
    GuestPath::new(p).unwrap()
}

/// A 512 MiB devcontainer sandbox named after the run and `tag`.
pub(crate) fn spec(settings: &Settings, tag: &str) -> SandboxSpec {
    SandboxSpec::new(
        name(settings, tag),
        ImageRef::new(DEBIAN_DEVCONTAINER).unwrap(),
    )
    .with_memory(MemoryMib::new(512).unwrap())
}

/// Runs `script` in the guest as root.
pub(crate) async fn sh<S: Sandbox>(sb: &S, script: &str) -> ExecOutput {
    sb.exec(ExecRequest::sh(script).as_user("root"))
        .await
        .expect("exec")
}

/// Accepts connections on `listener` and returns the first line of each.
pub(crate) fn collect_lines(mut listener: Listener) -> tokio::task::JoinHandle<Vec<String>> {
    tokio::spawn(async move {
        let mut lines = Vec::new();
        while let Ok(Ok(mut conn)) =
            tokio::time::timeout(Duration::from_secs(20), listener.accept()).await
        {
            let mut buf = Vec::new();
            let _ = tokio::time::timeout(Duration::from_secs(5), conn.read_to_end(&mut buf)).await;
            lines.push(String::from_utf8_lossy(&buf).trim_end().to_owned());
        }
        lines
    })
}

/// Stops and removes the named sandboxes, ignoring the ones already gone.
pub(crate) async fn cleanup(rt: &MsbRuntime, names: &[&SandboxName]) {
    for n in names {
        if let Ok(sb) = rt.get(n).await {
            let _ = sb.stop().await;
        }
        let _ = rt.remove(n).await;
    }
}

/// What a vsock sweep of one CID saw.
#[derive(Debug)]
pub(crate) struct Sweep {
    /// The ports that accepted the connection, in the order found.
    pub(crate) accepted: Vec<u32>,
    /// How the rest ended, as the probe summarised it (for the failure message and the log).
    pub(crate) summary: String,
}

/// Connects to every port in `ports` (numbers or `lo-hi` ranges) of vsock address `cid` from
/// inside the guest and sends `message` down each connection that is accepted.
pub(crate) async fn vsock_sweep<S: Sandbox>(
    sb: &S,
    cid: u32,
    message: &str,
    ports: &[&str],
) -> Sweep {
    let mut args = vec![
        "-e".to_owned(),
        VSOCK_SWEEP.to_owned(),
        message.to_owned(),
        cid.to_string(),
    ];
    args.extend(ports.iter().map(|p| (*p).to_owned()));
    let out = sb
        .exec(
            ExecRequest::new("perl", args)
                .as_user("root")
                .with_timeout(Duration::from_secs(600)),
        )
        .await
        .expect("vsock sweep");
    assert_eq!(out.status.code, 0, "sweep failed: {}", out.stderr_text());
    let accepted = out
        .stdout_text()
        .lines()
        .map(|l| l.strip_prefix("accepted ").unwrap().parse().unwrap())
        .collect();
    let summary = out.stderr_text().trim_end().to_owned();
    Sweep { accepted, summary }
}
