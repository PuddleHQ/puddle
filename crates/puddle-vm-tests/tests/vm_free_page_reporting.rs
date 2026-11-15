// SPDX-License-Identifier: AGPL-3.0-or-later
//! Memory a guest frees goes back to the host: the guest fills a large block of RAM, the host
//! process that runs the VM grows by about that much, the guest frees the block, and the process
//! shrinks again without the guest being stopped. This is the balloon device's free page
//! reporting working end to end in a real microVM, on Linux (KVM) and Windows (WHP); the device's
//! unit tests only show the device side.
//!
//! The measure is the resident set of the process that hosts the VM ([`resident_bytes`]), read
//! from outside, not anything the guest says. Runs only under `cargo nextest run --profile vm`.

use std::time::Duration;

use microsandbox::Sandbox;
use puddle_test_kit::eventually_async;
use puddle_vm_tests::{
    DEBIAN_DEVCONTAINER, HarnessError, VmEnv, create_from_registry, resident_bytes, within,
};

const MIB: u64 = 1024 * 1024;
/// What the guest fills and then frees.
const BLOCK_MIB: u64 = 512;
/// Guest RAM: the block fits with room for the guest's own use.
const GUEST_MEMORY_MIB: u32 = 2048;

const CREATE_BUDGET: Duration = Duration::from_secs(600);
const EXEC_BUDGET: Duration = Duration::from_secs(120);
const STOP_BUDGET: Duration = Duration::from_secs(60);
/// How long the host process may take to show the block (filling it is a guest write loop).
const GROW_WITHIN: Duration = Duration::from_secs(60);
/// How long the host process may take to give the block back: the guest kernel reports freed
/// pages after a short delay, then the device discards them. A limit on a hang, not a speed bar.
const SHRINK_WITHIN: Duration = Duration::from_secs(90);

/// A tmpfs holds the block: its pages are plain guest RAM, written (so the host backs them) and
/// freed at once when the file goes.
const FILL: &str = "mkdir -p /mnt/ballast && mount -t tmpfs -o size=768m tmpfs /mnt/ballast \
     && dd if=/dev/zero of=/mnt/ballast/block bs=1M count=512 2>&1";
const FREE: &str = "rm /mnt/ballast/block && umount /mnt/ballast";

fn mib(bytes: u64) -> u64 {
    bytes / MIB
}

async fn guest(sandbox: &Sandbox, what: &'static str, script: &str) -> Result<(), HarnessError> {
    let out = within(what, EXEC_BUDGET, sandbox.shell(script)).await??;
    assert!(
        out.status().success,
        "{what}: exit {}, stdout {:?}, stderr {:?}",
        out.status().code,
        String::from_utf8_lossy(out.stdout_bytes()),
        String::from_utf8_lossy(out.stderr_bytes()),
    );
    Ok(())
}

async fn host_pid(sandbox: &Sandbox) -> Result<u32, HarnessError> {
    let handle = Sandbox::get(sandbox.name()).await?;
    handle
        .local()
        .and_then(|local| local.pid)
        .and_then(|pid| u32::try_from(pid).ok())
        .ok_or_else(|| HarnessError::Io {
            action: "find the host process of",
            path: sandbox.name().into(),
            source: std::io::Error::other("the running sandbox has no process id"),
        })
}

/// Host resident memory, in bytes, at three points.
struct Readings {
    pid: u32,
    before: u64,
    peak: u64,
    after: u64,
}

async fn fill_and_free(env: &VmEnv) -> Result<Readings, HarnessError> {
    let builder = env
        .sandbox("freepage", DEBIAN_DEVCONTAINER)?
        .memory(GUEST_MEMORY_MIB);
    // Boxed: the SDK's futures are large (clippy::large_futures).
    env.scope(Box::pin(async {
        let sandbox =
            create_from_registry(DEBIAN_DEVCONTAINER, CREATE_BUDGET, builder.create()).await?;
        let pid = host_pid(&sandbox).await?;
        let block = BLOCK_MIB * MIB;
        let half = block / 2;
        let before = resident_bytes(pid)?;

        guest(&sandbox, "fill guest memory", FILL).await?;
        let peak = eventually_async(
            "the host process holds the block the guest filled",
            GROW_WITHIN,
            || async {
                let now = resident_bytes(pid).map_err(|e| e.to_string())?;
                if now >= before + half {
                    Ok(now)
                } else {
                    Err(format!(
                        "resident {} MiB, started at {} MiB, need {} MiB more",
                        mib(now),
                        mib(before),
                        mib(half)
                    ))
                }
            },
        )
        .await;

        guest(&sandbox, "free guest memory", FREE).await?;
        let after = eventually_async(
            "the host process gives back half the block",
            SHRINK_WITHIN,
            || async {
                let now = resident_bytes(pid).map_err(|e| e.to_string())?;
                if now + half <= peak {
                    Ok(now)
                } else {
                    Err(format!(
                        "resident {} MiB, peak {} MiB, need {} MiB or less",
                        mib(now),
                        mib(peak),
                        mib(peak - half)
                    ))
                }
            },
        )
        .await;

        within("stop", STOP_BUDGET, sandbox.stop()).await??;
        within("remove", STOP_BUDGET, Sandbox::remove(sandbox.name())).await??;
        Ok(Readings {
            pid,
            before,
            peak,
            after,
        })
    }))
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn vm_host_memory_drops_after_the_guest_frees_memory() {
    let env = VmEnv::from_env().await.expect("VM test environment");
    let result = Box::pin(fill_and_free(&env)).await;
    // Clean up whatever the run left, pass or fail; keep the home (msb logs) on failure.
    let cleaned = Box::pin(env.cleanup()).await;
    let seen = result.expect("fill and free guest memory");
    eprintln!(
        "host process {}: {} MiB before, {} MiB with the block, {} MiB after the guest freed it",
        seen.pid,
        mib(seen.before),
        mib(seen.peak),
        mib(seen.after)
    );
    cleaned.expect("cleanup");
    env.remove_home().await.expect("remove the private home");
}
