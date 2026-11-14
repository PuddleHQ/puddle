// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tier R: what a hosted runner cannot show, on the real machine (WHP, Defender, the machine's
//! own network). Each test boots a real devcontainer sandbox and asserts what only a working
//! boot produces; `ci/windows-e2e.ps1` runs them, the CI jobs leave them out
//! (`ci/vm-filter.sh`). Runs only under `cargo nextest run --profile vm`.

use std::time::Duration;

use microsandbox::Sandbox;
use puddle_vm_tests::{DEBIAN_DEVCONTAINER, HarnessError, VmEnv, create_from_registry, within};

/// Cold pull of the devcontainer image plus first boot (60 s on a workstation's first pull).
const CREATE_BUDGET: Duration = Duration::from_secs(600);
const EXEC_BUDGET: Duration = Duration::from_secs(90);
const STOP_BUDGET: Duration = Duration::from_secs(60);

/// What a guest command produced.
struct Ran {
    code: i32,
    stdout: String,
    stderr: String,
}

async fn run(sandbox: &Sandbox, what: &'static str, script: &str) -> Result<Ran, HarnessError> {
    let out = within(what, EXEC_BUDGET, sandbox.shell(script)).await??;
    Ok(Ran {
        code: out.status().code,
        stdout: String::from_utf8_lossy(out.stdout_bytes()).into_owned(),
        stderr: String::from_utf8_lossy(out.stderr_bytes()).into_owned(),
    })
}

/// Boots one sandbox, hands it to `body`, then stops and removes it. The run's VMs and private
/// home are cleaned up whether `body` passed or not; the home is kept on a failure (msb logs).
async fn with_booted<F>(name: &str, body: F)
where
    F: AsyncFnOnce(&Sandbox) -> Result<(), HarnessError>,
{
    let env = VmEnv::from_env().await.expect("VM test environment");
    let result = Box::pin(async {
        let builder = env.sandbox(name, DEBIAN_DEVCONTAINER)?;
        env.scope(Box::pin(async {
            let sandbox =
                create_from_registry(DEBIAN_DEVCONTAINER, CREATE_BUDGET, builder.create()).await?;
            let outcome = body(&sandbox).await;
            within("stop", STOP_BUDGET, sandbox.stop()).await??;
            within("remove", STOP_BUDGET, Sandbox::remove(sandbox.name())).await??;
            outcome
        }))
        .await
    })
    .await;
    let cleaned = Box::pin(env.cleanup()).await;
    result.expect("the booted sandbox's checks");
    cleaned.expect("cleanup");
    env.remove_home().await.expect("remove the private home");
}

/// The guest is a real Linux VM with its own kernel, CPUs and memory, and what it runs comes
/// back exactly: stdout, stderr and the exit code.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn vm_machine_boot_runs_a_real_guest() {
    with_booted("machine-boot", async |sandbox| {
        let kernel = run(sandbox, "uname", "uname -s -r").await?;
        assert_eq!(kernel.code, 0, "uname: {}", kernel.stderr);
        assert!(
            kernel.stdout.starts_with("Linux "),
            "the guest is not Linux: {:?}",
            kernel.stdout
        );

        let resources = run(
            sandbox,
            "resources",
            "echo cpus=$(nproc); awk '/MemTotal/ {print \"mem_kb=\" $2}' /proc/meminfo",
        )
        .await?;
        assert_eq!(resources.code, 0, "resources: {}", resources.stderr);
        let value = |key: &str| -> u64 {
            resources
                .stdout
                .lines()
                .find_map(|line| line.strip_prefix(key))
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or_else(|| panic!("no {key} in {:?}", resources.stdout))
        };
        assert!(value("cpus=") >= 1);
        assert!(value("mem_kb=") >= 256 * 1024, "{:?}", resources.stdout);

        let echoed = run(sandbox, "exit code", "echo out; echo err >&2; exit 7").await?;
        assert_eq!(echoed.code, 7);
        assert_eq!(echoed.stdout.trim(), "out");
        assert_eq!(echoed.stderr.trim(), "err");
        Ok(())
    })
    .await;
}

/// The guest reaches the machine's own network: a name resolves through the machine's resolver
/// and a TCP connection to the result opens. This is the path Defender and a corporate network
/// sit on, which a hosted runner does not have.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn vm_machine_guest_reaches_the_machines_network() {
    with_booted("machine-net", async |sandbox| {
        let lookup = run(
            sandbox,
            "name lookup",
            "getent ahostsv4 deb.debian.org | head -1",
        )
        .await?;
        assert_eq!(
            lookup.code, 0,
            "no answer for deb.debian.org: {}",
            lookup.stderr
        );
        let address = lookup
            .stdout
            .split_whitespace()
            .next()
            .unwrap_or_else(|| panic!("empty lookup: {:?}", lookup.stdout))
            .to_owned();
        assert!(
            address.parse::<std::net::Ipv4Addr>().is_ok(),
            "not an address: {address:?}"
        );

        let connect = run(
            sandbox,
            "tcp connect",
            &format!("timeout 20 bash -c 'exec 3<>/dev/tcp/{address}/80 && echo connected'"),
        )
        .await?;
        assert_eq!(
            connect.code, 0,
            "connect to {address}:80: {}",
            connect.stderr
        );
        assert_eq!(connect.stdout.trim(), "connected");
        Ok(())
    })
    .await;
}
