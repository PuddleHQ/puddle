// SPDX-License-Identifier: AGPL-3.0-or-later
//! A guest that floods the host-side devices meets the caps of the msb fork, and the sandbox stays
//! healthy. The caps are unit-tested in the fork at device level; this runs them from inside a
//! booted guest: it reads 64 MiB from the random-number device, writes 32 MiB to its console and
//! opens file handles past the per-mount limit. Seen from outside, as the fork's own tests cannot:
//!
//! - the process that hosts the VM keeps its memory (resident set within a bound of what it had
//!   before the floods);
//! - `kernel.log` stops at 16 MiB plus the fork's marker line, though the guest saw every write succeed;
//! - the handle flood ends in `EMFILE` at the mount's limit, not at the guest's own;
//! - the guest still answers and the sandbox stops cleanly, its host process gone.
//!
//! Linux only: the host process is read from `/proc`, and the test file is not built elsewhere.
//! Windows (WHP) has the same caps and is not run. Runs only under `cargo nextest run --profile vm`.
#![cfg(target_os = "linux")]

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::time::Duration;

use microsandbox::Sandbox;
use puddle_test_kit::eventually_async;
use puddle_vm_tests::{
    DEBIAN_DEVCONTAINER, HarnessError, VmEnv, create_from_registry, resident_bytes, within,
};

const MIB: u64 = 1024 * 1024;
const CREATE_BUDGET: Duration = Duration::from_secs(600);
/// A flood is a guest loop of a few hundred thousand calls; this is a limit on a hang.
const FLOOD_BUDGET: Duration = Duration::from_secs(300);
const EXEC_BUDGET: Duration = Duration::from_secs(60);
const STOP_BUDGET: Duration = Duration::from_secs(60);
/// How long the host may take to show the effect of a flood it was sent (log written, process settled).
const SETTLE_WITHIN: Duration = Duration::from_secs(60);

/// The fork's console log cap, spelled out here so that changing the constant fails this test.
const KERNEL_LOG_CAP: u64 = 16 * MIB;
/// What the fork writes once, at the cap, in place of the rest.
const KERNEL_LOG_MARKER: &[u8] = b"\n[console log capped by the host; later output dropped]\n";

const RNG_BYTES: u64 = 64 * MIB;
/// The fork's per-mount handle limit (`max_open_handles`), spelled out like the log cap.
const HANDLE_LIMIT: u64 = 65_536;
/// The guest's own descriptor limit for the handle flood: well above [`HANDLE_LIMIT`], so a stop
/// at the limit is the host's refusal and not the guest's.
const GUEST_FD_LIMIT: u64 = 300_000;
/// What the flood loop tries to open: past [`HANDLE_LIMIT`] with room to show a missing cap.
const HANDLE_TRIES: u64 = 120_000;
/// Memory the host process may gain over its size before the floods. The handle flood makes the
/// guest kernel cache tens of thousands of inodes, so this is generous; an uncapped rng request
/// (a 4 GiB buffer) or an unbounded handle table is far beyond it.
const RSS_GROWTH_BOUND: u64 = 384 * MIB;

/// 64 MiB through the guest's hardware random-number driver, which is the host's rng device.
/// `wc -c` counts what the guest got, so a device that stops answering fails here.
const RNG_FLOOD: &str = "head -c 67108864 /dev/hwrng | wc -c";
/// 32 MiB of text to the guest's console, which msb routes to `kernel.log`.
const CONSOLE_FLOOD: &str =
    "head -c 33554432 /dev/zero | tr '\\0' 'x' > /dev/console && echo console-flood-done";
/// Opens `/bin/true` over and over without closing, as root so the descriptor limit can be raised.
/// Only shell builtins run after the flood: once the mount's handles are gone, starting a program
/// from it fails too. Prints the opens that worked and the shell's own words for the refusal.
const HANDLE_FLOOD: &str = "ulimit -n 300000 || exit 9; n=0; \
     while [ $n -lt 120000 ]; do exec {fd}</flood/target 2>/dev/null || break; n=$((n+1)); done; \
     err=$( { : </flood/target; } 2>&1 ); echo \"opened=$n err=$err\"";

fn mib(bytes: u64) -> u64 {
    bytes / MIB
}

/// Runs a script in the guest as one `bash -c`, returns its stdout.
async fn guest(
    sandbox: &Sandbox,
    what: &'static str,
    script: &str,
) -> Result<String, HarnessError> {
    let out = within(what, FLOOD_BUDGET, sandbox.exec("bash", ["-c", script])).await??;
    let stdout = String::from_utf8_lossy(out.stdout_bytes()).into_owned();
    assert!(
        out.status().success,
        "{what}: exit {}, stdout {stdout:?}, stderr {:?}",
        out.status().code,
        String::from_utf8_lossy(out.stderr_bytes()),
    );
    Ok(stdout)
}

/// The last `KERNEL_LOG_MARKER.len()` bytes of the file and its length, or what is there if less.
fn log_tail(path: &Path) -> std::io::Result<(u64, Vec<u8>)> {
    let mut file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();
    let want = KERNEL_LOG_MARKER.len() as u64;
    file.seek(SeekFrom::Start(len.saturating_sub(want)))?;
    let mut tail = Vec::new();
    file.read_to_end(&mut tail)?;
    Ok((len, tail))
}

/// What the run saw, printed by the test.
struct Seen {
    rss_before: u64,
    rss_after: u64,
    kernel_log_len: u64,
    handles_opened: u64,
}

async fn flood(env: &VmEnv) -> Result<Seen, HarnessError> {
    // The folder the guest floods with opens: a host mount is served by the passthrough file system
    // whose handle limit is under test.
    let shared = tempfile::tempdir().map_err(|source| HarnessError::Io {
        action: "create",
        path: "a temporary folder".into(),
        source,
    })?;
    let target = shared.path().join("target");
    std::fs::write(&target, b"x").map_err(|source| HarnessError::Io {
        action: "write",
        path: target.clone(),
        source,
    })?;
    let builder = env
        .sandbox("flood", DEBIAN_DEVCONTAINER)?
        .volume("/flood", |m| m.bind(shared.path()));
    // Boxed: the SDK's futures are large (clippy::large_futures).
    env.scope(Box::pin(async {
        let sandbox =
            create_from_registry(DEBIAN_DEVCONTAINER, CREATE_BUDGET, builder.create()).await?;
        let pid = env.process_id_of(sandbox.name()).await?;
        let kernel_log = env.log_dir(sandbox.name()).join("kernel.log");
        let rss_before = resident_bytes(pid)?;

        let rng = guest(&sandbox, "rng flood", RNG_FLOOD).await?;
        assert_eq!(
            rng.trim(),
            RNG_BYTES.to_string(),
            "the guest did not get all of the random bytes it asked for"
        );

        let console = guest(&sandbox, "console flood", CONSOLE_FLOOD).await?;
        assert!(console.contains("console-flood-done"), "{console:?}");

        // The log writer runs on the host a moment behind the guest: wait for the marker, then
        // the whole file is the cap and the marker, no more.
        let kernel_log_len = eventually_async(
            "kernel.log ends at the cap with the marker line",
            SETTLE_WITHIN,
            || async {
                let (len, tail) = log_tail(&kernel_log).map_err(|e| e.to_string())?;
                if tail == KERNEL_LOG_MARKER {
                    Ok(len)
                } else {
                    Err(format!(
                        "kernel.log is {} MiB ({len} bytes) and does not end with the marker",
                        mib(len)
                    ))
                }
            },
        )
        .await;
        assert_eq!(
            kernel_log_len,
            KERNEL_LOG_CAP + KERNEL_LOG_MARKER.len() as u64,
            "kernel.log must be the cap plus one marker line"
        );

        let handles = guest(&sandbox, "handle flood", HANDLE_FLOOD).await?;
        let handles_opened = parse_opened(&handles).unwrap_or(0);
        assert!(
            handles.contains("Too many open files"),
            "the flood must end in the host's EMFILE refusal, saw {handles:?}"
        );
        // Not lower than the limit less the handles the guest holds itself, never past it.
        assert!(
            (HANDLE_LIMIT - 1024..=HANDLE_LIMIT).contains(&handles_opened),
            "{handles_opened} opens worked (output {handles:?}); the mount's limit is {HANDLE_LIMIT} (guest limit {GUEST_FD_LIMIT}, tried {HANDLE_TRIES})"
        );

        // The host process settles back within the bound once the guest's handles are gone with
        // its shell, and it was never above it for the other floods: the bound holds at the end.
        let rss_after = eventually_async(
            "the host process stays within its memory bound",
            SETTLE_WITHIN,
            || async {
                let now = resident_bytes(pid).map_err(|e| e.to_string())?;
                if now <= rss_before + RSS_GROWTH_BOUND {
                    Ok(now)
                } else {
                    Err(format!(
                        "resident {} MiB, was {} MiB before the floods, bound +{} MiB",
                        mib(now),
                        mib(rss_before),
                        mib(RSS_GROWTH_BOUND)
                    ))
                }
            },
        )
        .await;

        // The guest still answers, and nothing was lost to the floods.
        let alive = guest(&sandbox, "answer after the floods", "echo still-here").await?;
        assert_eq!(alive.trim(), "still-here");

        within("stop", STOP_BUDGET, sandbox.stop()).await??;
        eventually_async("the VM process is gone after stop", STOP_BUDGET, || async {
            match resident_bytes(pid) {
                Err(_) => Ok(()),
                Ok(rss) => Err(format!("process {pid} still resident: {} MiB", mib(rss))),
            }
        })
        .await;
        within("remove", EXEC_BUDGET, Sandbox::remove(sandbox.name())).await??;
        Ok(Seen {
            rss_before,
            rss_after,
            kernel_log_len,
            handles_opened,
        })
    }))
    .await
}

/// The number after `opened=` in the handle flood's output.
fn parse_opened(out: &str) -> Option<u64> {
    out.split_whitespace()
        .find_map(|w| w.strip_prefix("opened="))
        .and_then(|n| n.parse().ok())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn vm_guest_floods_meet_the_host_caps_and_the_sandbox_stays_healthy() {
    let env = VmEnv::from_env().await.expect("VM test environment");
    let result = Box::pin(flood(&env)).await;
    // Clean up whatever the run left, pass or fail; keep the home (msb logs) on failure.
    let cleaned = Box::pin(env.cleanup()).await;
    let seen = result.expect("flood the host from the guest");
    eprintln!(
        "host process {} MiB before the floods, {} MiB after; kernel.log {} bytes; {} handles opened",
        mib(seen.rss_before),
        mib(seen.rss_after),
        seen.kernel_log_len,
        seen.handles_opened
    );
    cleaned.expect("cleanup");
    env.remove_home().await.expect("remove the private home");
}
