// SPDX-License-Identifier: AGPL-3.0-or-later
//! Shared setup of the VM tests: the adapter on the VM harness's private msb home. Test-only:
//! `puddle-compute-msb` lists this crate under `[dev-dependencies]`. The helpers live in a library so
//! that every one is `pub` and none is dead in a test binary that uses a different subset.
#![forbid(unsafe_code)]
#![expect(
    clippy::missing_panics_doc,
    reason = "helpers fail the test that called them by panicking"
)]
#![expect(
    clippy::must_use_candidate,
    missing_docs,
    reason = "test helpers: the names and signatures carry the meaning, and a dropped result fails the test"
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

pub mod mount_attack;

/// A throwaway public key (its private half was never kept): the SDK's SSH server refuses to
/// start without an authorized key.
pub const TEST_SSH_KEY: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOrqAVBGG1BD2K0dsi6Z6il4+PlI8R/egmhi8Ox00c0/ puddle-contract-test";

/// The harness settings (runtime pair, run prefix, private home) from the environment.
pub fn settings() -> Settings {
    Settings::from_lookup(|var| std::env::var(var).ok()).expect("VM test settings")
}

/// The adapter on the run's private msb home, with `<home>/guest-share` as the mount root. Its
/// warnings (a lost boot race, with msb's log tail) go to the test's stderr.
pub async fn runtime(settings: &Settings) -> MsbRuntime {
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
pub fn name(settings: &Settings, tag: &str) -> SandboxName {
    SandboxName::new(&format!("{}-{tag}", settings.prefix)).unwrap()
}

/// A guest path that must be valid.
pub fn path(p: &str) -> GuestPath {
    GuestPath::new(p).unwrap()
}

/// A 512 MiB devcontainer sandbox named after the run and `tag`.
pub fn spec(settings: &Settings, tag: &str) -> SandboxSpec {
    SandboxSpec::new(
        name(settings, tag),
        ImageRef::new(DEBIAN_DEVCONTAINER).unwrap(),
    )
    .with_memory(MemoryMib::new(512).unwrap())
}

/// Runs `script` in the guest as root.
pub async fn sh<S: Sandbox>(sb: &S, script: &str) -> ExecOutput {
    let out = sb
        .exec(ExecRequest::sh(script).as_user("root"))
        .await
        .expect("exec");
    assert!(
        !out.stdout_truncated && !out.stderr_truncated,
        "the guest script's output was cut at the exec output cap:\n{script}"
    );
    out
}

/// How long a connection may take to send its line and close before the collector reports it.
const LINE_READ_BOUND: Duration = Duration::from_secs(5);

/// How long the collector keeps accepting after the stop signal, so a connection already queued
/// on the endpoint is not lost.
const DRAIN_IDLE: Duration = Duration::from_millis(250);

/// Collects the first line of each connection that reaches an endpoint, until the test says it
/// is done.
pub struct LineCollector {
    stop: tokio::sync::oneshot::Sender<()>,
    task: tokio::task::JoinHandle<Result<Vec<String>, String>>,
}

impl LineCollector {
    /// Signals that the senders are finished (the sandboxes are stopped) and returns the lines
    /// in arrival order. Connections queued before the signal are still taken.
    ///
    /// # Errors
    ///
    /// When a connection did not send and close within its bound, or the accept itself failed: a
    /// short or missing line then says so instead of passing for what the sender sent.
    pub async fn finish(self) -> Result<Vec<String>, String> {
        let _ = self.stop.send(());
        self.task
            .await
            .map_err(|e| format!("collector task: {e}"))?
    }
}

/// Accepts connections on `listener` until [`LineCollector::finish`] and keeps the first line
/// of each. There is no idle bound: the VMs boot for a time that varies, so only the test knows
/// when no more can come.
pub fn collect_lines(listener: Listener) -> LineCollector {
    collect_lines_within(listener, LINE_READ_BOUND)
}

fn collect_lines_within(mut listener: Listener, read_bound: Duration) -> LineCollector {
    let (stop, mut stopped) = tokio::sync::oneshot::channel::<()>();
    let task = tokio::spawn(async move {
        let mut lines = Vec::new();
        let mut problems = Vec::new();
        let mut draining = false;
        loop {
            let conn = if draining {
                match tokio::time::timeout(DRAIN_IDLE, listener.accept()).await {
                    Ok(conn) => conn,
                    Err(_) => break,
                }
            } else {
                tokio::select! {
                    _ = &mut stopped => {
                        draining = true;
                        continue;
                    }
                    conn = listener.accept() => conn,
                }
            };
            let mut conn = match conn {
                Ok(conn) => conn,
                Err(e) => {
                    problems.push(format!("accept failed: {e}"));
                    break;
                }
            };
            let mut buf = Vec::new();
            match tokio::time::timeout(read_bound, conn.read_to_end(&mut buf)).await {
                Ok(Ok(_)) => {}
                Ok(Err(e)) => problems.push(format!("read failed after {buf:?}: {e}")),
                Err(_) => problems.push(format!(
                    "read timed out after {read_bound:?} with {:?} received",
                    String::from_utf8_lossy(&buf)
                )),
            }
            lines.push(String::from_utf8_lossy(&buf).trim_end().to_owned());
        }
        if problems.is_empty() {
            Ok(lines)
        } else {
            Err(format!("{problems:#?}; lines so far: {lines:?}"))
        }
    });
    LineCollector { stop, task }
}

/// Stops and removes the named sandboxes, ignoring the ones already gone.
pub async fn cleanup(rt: &MsbRuntime, names: &[&SandboxName]) {
    for n in names {
        if let Ok(sb) = rt.get(n).await {
            let _ = sb.stop().await;
        }
        let _ = rt.remove(n).await;
    }
}

/// What a vsock sweep of one CID saw.
#[derive(Debug)]
pub struct Sweep {
    /// The ports that accepted the connection, in the order found.
    pub accepted: Vec<u32>,
    /// How the rest ended, as the probe summarised it (for the failure message and the log).
    pub summary: String,
}

/// Connects to every port in `ports` (numbers or `lo-hi` ranges) of vsock address `cid` from
/// inside the guest and sends `message` down each connection that is accepted.
pub async fn vsock_sweep<S: Sandbox>(sb: &S, cid: u32, message: &str, ports: &[&str]) -> Sweep {
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

#[cfg(test)]
mod tests;
