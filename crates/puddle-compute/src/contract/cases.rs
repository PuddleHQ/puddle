// SPDX-License-Identifier: AGPL-3.0-or-later
//! The contract cases. Each is an `async fn(&Case<R>) -> Result<(), String>`; the `Err` says
//! what differed from the contract.

use std::future::Future;
use std::time::{Duration, Instant};

use puddle_types::{
    CpuCount, GuestEnv, GuestPath, ImageRef, MemoryMib, SandboxName, WorkspaceStatus,
};
use tokio::io::AsyncReadExt;

use self::hostile::{
    hg01_guest_has_no_direct_network, hg01_raw_and_packet_sockets_reach_no_network,
    hg02_no_other_vsock_address_accepts, hg02_only_the_routed_vsock_port_accepts,
};
use self::settings::{
    cpus_change_applies_at_next_start, env_change_applies_at_next_start_and_keeps_the_root_disk,
    memory_change_applies_at_next_start, routes_and_no_network_are_accepted,
    ssh_server_speaks_first,
};
use super::{Case, STEP_TIMEOUT};
use crate::{
    ComputeError, DiskSize, ExecOutput, ExecRequest, FileMount, OwnedDisk, Runtime, Sandbox,
    SandboxSpec, VolumeMount, VolumeSpec, VsockRoute,
};

type Outcome = Result<(), String>;

/// Fails the case with a formatted message unless `cond` holds.
macro_rules! check {
    ($cond:expr, $($msg:tt)+) => {
        if !$cond {
            return Err(format!($($msg)+));
        }
    };
}

mod hostile;
mod settings;

/// Declares the cases once: [`CASES`], [`CASE_NOTES`] and the dispatcher come from this list.
macro_rules! cases {
    ($($name:ident => $note:literal,)*) => {
        /// Every contract case, in run order.
        pub const CASES: &[&str] = &[$(stringify!($name)),*];

        /// Per case: the behaviour it pins. A runtime that is not the fake runs every case in the
        /// VM lanes on each merge, so a note is a claim those runs check, not a version's memory.
        pub const CASE_NOTES: &[(&str, &str)] = &[$((stringify!($name), $note)),*];

        pub(super) async fn dispatch<R: Runtime>(case: &Case<'_, R>, name: &str) -> Outcome {
            match name {
                // Boxed: one inline future per case would make this one as large as the largest.
                $(stringify!($name) => Box::pin($name(case)).await,)*
                _ => Err(format!("no such contract case {name}")),
            }
        }
    };
}

cases! {
    probe_reports_runtime_version => "the runtime's version string is readable",
    pull_image_returns_its_config => "image config facts (entrypoint, command, env, user); an unknown image fails",
    unknown_image_fails_create_cleanly => "a create with an unknown image fails and leaves nothing behind",
    create_boots_and_returns_owning_handle => "create returns a running owning handle that list shows; puddle_owned comes from the owner label",
    create_refuses_a_taken_name => "a second create under a taken name is refused",
    invalid_spec_is_refused_before_anything_exists => "Puddle-side check (SandboxSpec::validate)",
    stop_then_start_boots_again => "status after stop and after a restart",
    dropping_the_owning_handle_stops_the_vm => "dropping the owner handle shuts the VM down; the record reads Stopped or Crashed, and the case accepts either",
    get_readopts_a_running_sandbox_without_owning_it => "get re-adopts a running sandbox without owning it",
    unknown_names_are_not_found => "an unknown or removed name is not found",
    remove_needs_a_stopped_sandbox_and_frees_the_name => "remove refuses a running sandbox and frees the name of a stopped one",
    kill_ends_a_vm_by_name_and_is_idempotent => "kill ends a running VM without a handle's help, a down one is a no-op, an unknown name is not found",
    exec_exit_codes_are_exact => "exit codes 0, 3 and 127 come through exactly",
    signal_killed_exec_is_a_failure => "a SIGKILLed command reports a failure code, not success",
    exec_timeout_is_enforced => "a command that outlives its timeout fails with ExecTimeout",
    exec_needs_a_running_sandbox => "exec on a stopped sandbox is refused",
    create_env_reaches_exec => "create-time env is visible in exec",
    stdin_reaches_exec => "stdin bytes reach the command",
    file_mount_is_read_only => "a file mount is read-only in the guest",
    dir_mount_is_read_only => "a directory mount is read-only in the guest",
    root_disk_survives_restart_not_remove => "root disk files persist across a restart and go with remove",
    owned_disk_survives_restart_not_remove => "an owned disk is kept across a restart and goes with the sandbox",
    named_volume_survives_restart_and_remove => "volume data is kept across restart, recreate and remove; a plain named mount reattaches it",
    volume_capacity_reads_back => "a volume's capacity reads back",
    second_attach_is_refused_naming_the_holder => "a second attach is refused and leaves a Stopped record; the holder is named by Puddle (ADR 0006 point 8)",
    failed_create_leaves_no_stale_dir => "a create that fails its volume check leaves no stale directory behind",
    missing_volume_fails_create => "a named volume that does not exist fails create and is not created implicitly",
    ssh_server_speaks_first => "the SSH banner arrives first, also with several first connections at once; full SSH exec is covered by the VM tests",
    routes_and_no_network_are_accepted => "a spec with vsock routes is accepted and boots",
    memory_change_applies_at_next_start => "the memory setting: modify().memory().next_start() on msb",
    cpus_change_applies_at_next_start => "the cpus setting: modify().cpus().next_start() on msb",
    env_change_applies_at_next_start_and_keeps_the_root_disk => "modify().env()/remove_env().next_start() on msb: the new value is seen and the root disk stays",
    hg01_guest_has_no_direct_network => "as root, direct IP, DNS, UDP, IPv6 and raw ICMP reach nothing while loopback works",
    hg01_raw_and_packet_sockets_reach_no_network => "as root, raw IP and AF_PACKET sockets reach nothing, also over a sink device given an address and a route",
    hg02_only_the_routed_vsock_port_accepts => "a vsock connect sweep of the host and CIDs 0, 1 and 3: only the routed port accepts, and its bytes arrive",
    hg02_no_other_vsock_address_accepts => "a vsock sweep of other guests' CIDs, the largest CIDs, CID_ANY and the guest's own CID: none accepts",
}

/// Awaits a runtime call with [`STEP_TIMEOUT`] and turns its error into a case failure.
async fn step<T>(
    what: &str,
    call: impl Future<Output = Result<T, ComputeError>>,
) -> Result<T, String> {
    match tokio::time::timeout(STEP_TIMEOUT, call).await {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => Err(format!("{what}: unexpected error: {e}")),
        Err(_) => Err(format!("{what}: no answer within {STEP_TIMEOUT:?}")),
    }
}

/// Awaits a runtime call that must fail and returns its error.
async fn fails<T>(
    what: &str,
    call: impl Future<Output = Result<T, ComputeError>>,
) -> Result<ComputeError, String> {
    match tokio::time::timeout(STEP_TIMEOUT, call).await {
        Ok(Ok(_)) => Err(format!("{what}: succeeded but must fail")),
        Ok(Err(e)) => Ok(e),
        Err(_) => Err(format!("{what}: no answer within {STEP_TIMEOUT:?}")),
    }
}

fn path(p: &str) -> Result<GuestPath, String> {
    GuestPath::new(p).map_err(|e| e.to_string())
}

/// A minimal spec: the test image with 512 MiB, so several case VMs fit on a CI runner.
fn spec<R: Runtime>(c: &Case<'_, R>, name: &SandboxName) -> SandboxSpec {
    let memory = MemoryMib::new(512).unwrap_or(MemoryMib::DEFAULT);
    SandboxSpec::new(name.clone(), c.image().clone()).with_memory(memory)
}

async fn sh<S: Sandbox>(sb: &S, script: &str) -> Result<ExecOutput, String> {
    step(
        &format!("exec {script:?}"),
        sb.exec(ExecRequest::sh(script).as_user("root")),
    )
    .await
}

async fn write_file<S: Sandbox>(sb: &S, file: &str, data: &str) -> Result<(), String> {
    let out = step(
        &format!("write {file}"),
        sb.exec(
            ExecRequest::new("tee", [file])
                .with_stdin(data)
                .as_user("root"),
        ),
    )
    .await?;
    check!(
        out.status.success(),
        "writing {file} failed: {} {}",
        out.status.code,
        out.stderr_text()
    );
    Ok(())
}

async fn read_file<S: Sandbox>(sb: &S, file: &str) -> Result<Option<String>, String> {
    let out = step(
        &format!("read {file}"),
        sb.exec(ExecRequest::new("cat", [file]).as_user("root")),
    )
    .await?;
    Ok(out.status.success().then(|| out.stdout_text().into_owned()))
}

async fn status_in_list<R: Runtime>(
    rt: &R,
    name: &SandboxName,
) -> Result<Option<WorkspaceStatus>, String> {
    Ok(step("list", rt.list())
        .await?
        .into_iter()
        .find(|i| i.name == name.as_str())
        .map(|i| i.status))
}

async fn probe_reports_runtime_version<R: Runtime>(c: &Case<'_, R>) -> Outcome {
    let caps = step("probe", c.runtime().probe()).await?;
    check!(!caps.runtime_version.is_empty(), "empty runtime version");
    Ok(())
}

fn bogus_image<R: Runtime>(c: &Case<'_, R>) -> Result<ImageRef, String> {
    ImageRef::new(&format!("{}.invalid/none:0", c.scope)).map_err(|e| e.to_string())
}

async fn pull_image_returns_its_config<R: Runtime>(c: &Case<'_, R>) -> Outcome {
    let config = step("pull test image", c.runtime().pull_image(c.image())).await?;
    check!(
        config.env_var("PATH").is_some_and(|p| p.contains("/bin")),
        "image config has no PATH: {config:?}"
    );
    let err = fails(
        "pull unknown image",
        c.runtime().pull_image(&bogus_image(c)?),
    )
    .await?;
    check!(
        matches!(err, ComputeError::ImagePull { .. }),
        "want ImagePull, got {err:?}"
    );
    Ok(())
}

async fn unknown_image_fails_create_cleanly<R: Runtime>(c: &Case<'_, R>) -> Outcome {
    let name = c.sandbox("img")?;
    let mut s = spec(c, &name);
    s.image = bogus_image(c)?;
    let err = fails("create with unknown image", c.runtime().create(s)).await?;
    check!(
        matches!(err, ComputeError::ImagePull { .. }),
        "want ImagePull, got {err:?}"
    );
    check!(
        status_in_list(c.runtime(), &name).await?.is_none(),
        "a sandbox record was left behind"
    );
    Ok(())
}

async fn create_boots_and_returns_owning_handle<R: Runtime>(c: &Case<'_, R>) -> Outcome {
    let name = c.sandbox("a")?;
    let sb = step("create", c.runtime().create(spec(c, &name))).await?;
    check!(sb.name() == &name, "handle names {}", sb.name());
    check!(sb.owns_lifecycle(), "create must return an owning handle");
    let status = step("status", sb.status()).await?;
    check!(
        status == WorkspaceStatus::Running,
        "status {status} after create"
    );
    let listed = status_in_list(c.runtime(), &name).await?;
    check!(
        listed == Some(WorkspaceStatus::Running),
        "list shows {listed:?}"
    );
    let owned = step("list", c.runtime().list())
        .await?
        .into_iter()
        .find(|i| i.name == name.as_str())
        .is_some_and(|i| i.puddle_owned);
    check!(owned, "list doesn't mark Puddle's sandbox as Puddle-owned");
    Ok(())
}

async fn create_refuses_a_taken_name<R: Runtime>(c: &Case<'_, R>) -> Outcome {
    let name = c.sandbox("a")?;
    let _first = step("create", c.runtime().create(spec(c, &name))).await?;
    let err = fails("second create", c.runtime().create(spec(c, &name))).await?;
    check!(
        matches!(err, ComputeError::AlreadyExists { ref sandbox } if sandbox == name.as_str()),
        "want AlreadyExists naming {name}, got {err:?}"
    );
    Ok(())
}

async fn invalid_spec_is_refused_before_anything_exists<R: Runtime>(c: &Case<'_, R>) -> Outcome {
    let name = c.sandbox("bad")?;
    let s = spec(c, &name)
        .with_route(VsockRoute::new(5000, c.route_endpoint("a")))
        .with_route(VsockRoute::new(5000, c.route_endpoint("b")));
    let err = fails("create with a port routed twice", c.runtime().create(s)).await?;
    check!(
        matches!(err, ComputeError::InvalidSpec { .. }),
        "want InvalidSpec, got {err:?}"
    );
    check!(
        status_in_list(c.runtime(), &name).await?.is_none(),
        "a record was left"
    );
    let stale = step("stale dirs", c.runtime().stale_dirs()).await?;
    check!(
        !stale.iter().any(|d| d == name.as_str()),
        "a stale dir was left"
    );
    Ok(())
}

async fn stop_then_start_boots_again<R: Runtime>(c: &Case<'_, R>) -> Outcome {
    let rt = c.runtime();
    let name = c.sandbox("a")?;
    let first = step("create", rt.create(spec(c, &name))).await?;
    step("stop", first.stop()).await?;
    let status = step("status", first.status()).await?;
    check!(
        status == WorkspaceStatus::Stopped,
        "status {status} after stop"
    );
    let listed = status_in_list(rt, &name).await?;
    check!(
        listed == Some(WorkspaceStatus::Stopped),
        "list shows {listed:?} after stop"
    );
    step("second stop", first.stop()).await?;
    let second = step("start", rt.start(&name)).await?;
    check!(
        second.owns_lifecycle(),
        "start must return an owning handle"
    );
    let status = step("status", second.status()).await?;
    check!(
        status == WorkspaceStatus::Running,
        "status {status} after start"
    );
    let out = sh(&second, "exit 0").await?;
    check!(
        out.status.success(),
        "exec after start: {}",
        out.status.code
    );
    check!(
        first.exec(ExecRequest::sh("exit 0")).await.is_err(),
        "the handle of the earlier boot still runs commands"
    );
    Ok(())
}

async fn dropping_the_owning_handle_stops_the_vm<R: Runtime>(c: &Case<'_, R>) -> Outcome {
    let rt = c.runtime();
    let name = c.sandbox("a")?;
    drop(step("create", rt.create(spec(c, &name))).await?);
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let listed = status_in_list(rt, &name).await?;
        if listed.is_some_and(WorkspaceStatus::is_down) {
            break;
        }
        check!(
            Instant::now() < deadline,
            "still {listed:?} 30 s after dropping the owning handle"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let again = step("start after crash", rt.start(&name)).await?;
    let out = sh(&again, "exit 0").await?;
    check!(out.status.success(), "exec after restart failed");
    Ok(())
}

async fn get_readopts_a_running_sandbox_without_owning_it<R: Runtime>(c: &Case<'_, R>) -> Outcome {
    let rt = c.runtime();
    let name = c.sandbox("a")?;
    let owner = step("create", rt.create(spec(c, &name))).await?;
    let adopted = step("get", rt.get(&name)).await?;
    check!(!adopted.owns_lifecycle(), "get must not own the VM");
    let out = sh(&adopted, "exit 0").await?;
    check!(
        out.status.success(),
        "exec through the adopted handle failed"
    );
    drop(adopted);
    let status = step("status", owner.status()).await?;
    check!(
        status == WorkspaceStatus::Running,
        "dropping a non-owning handle changed the state to {status}"
    );
    step("stop", owner.stop()).await?;
    let err = fails("get a stopped sandbox", rt.get(&name)).await?;
    check!(
        matches!(err, ComputeError::InvalidState { .. }),
        "want InvalidState, got {err:?}"
    );
    Ok(())
}

async fn unknown_names_are_not_found<R: Runtime>(c: &Case<'_, R>) -> Outcome {
    let rt = c.runtime();
    let name = c.sandbox("nope")?;
    let not_found = |e: &ComputeError| matches!(e, ComputeError::NotFound { .. });
    let e = fails("get", rt.get(&name)).await?;
    check!(not_found(&e), "get: want NotFound, got {e:?}");
    let e = fails("start", rt.start(&name)).await?;
    check!(not_found(&e), "start: want NotFound, got {e:?}");
    let e = fails("remove", rt.remove(&name)).await?;
    check!(not_found(&e), "remove: want NotFound, got {e:?}");
    let e = fails("remove stale dir", rt.remove_stale_dir(&name)).await?;
    check!(not_found(&e), "remove_stale_dir: want NotFound, got {e:?}");
    let vol = c.volume("nope")?;
    let info = step("volume", rt.volume(&vol)).await?;
    check!(info.is_none(), "volume() found {info:?}");
    let e = fails("remove volume", rt.remove_volume(&vol)).await?;
    check!(
        matches!(e, ComputeError::VolumeNotFound { .. }),
        "remove_volume: want VolumeNotFound, got {e:?}"
    );
    Ok(())
}

async fn remove_needs_a_stopped_sandbox_and_frees_the_name<R: Runtime>(c: &Case<'_, R>) -> Outcome {
    let rt = c.runtime();
    let name = c.sandbox("a")?;
    let sb = step("create", rt.create(spec(c, &name))).await?;
    let err = fails("remove while running", rt.remove(&name)).await?;
    check!(
        matches!(err, ComputeError::InvalidState { .. }),
        "want InvalidState, got {err:?}"
    );
    step("stop", sb.stop()).await?;
    step("remove", rt.remove(&name)).await?;
    check!(
        status_in_list(rt, &name).await?.is_none(),
        "still listed after remove"
    );
    let _again = step("create again", rt.create(spec(c, &name))).await?;
    Ok(())
}

async fn kill_ends_a_vm_by_name_and_is_idempotent<R: Runtime>(c: &Case<'_, R>) -> Outcome {
    let rt = c.runtime();
    let name = c.sandbox("a")?;
    let _sb = step("create", rt.create(spec(c, &name))).await?;
    step("kill", rt.kill(&name)).await?;
    let after = status_in_list(rt, &name).await?;
    check!(
        after.is_some_and(WorkspaceStatus::is_down),
        "after kill the sandbox reads {after:?}, want a down state"
    );
    step("kill again", rt.kill(&name)).await?;
    let gone = c.sandbox("never-created")?;
    let err = fails("kill unknown", rt.kill(&gone)).await?;
    check!(
        matches!(err, ComputeError::NotFound { .. }),
        "want NotFound, got {err:?}"
    );
    Ok(())
}

async fn exec_exit_codes_are_exact<R: Runtime>(c: &Case<'_, R>) -> Outcome {
    let sb = step("create", c.runtime().create(spec(c, &c.sandbox("a")?))).await?;
    for (script, want) in [("exit 0", 0), ("exit 3", 3), ("nonexistent-cmd-xyz", 127)] {
        let out = sh(&sb, script).await?;
        check!(
            out.status.code == want,
            "{script:?} exited {} (want {want})",
            out.status.code
        );
        check!(
            out.status.success() == (want == 0),
            "success() wrong for {script:?}"
        );
    }
    Ok(())
}

async fn signal_killed_exec_is_a_failure<R: Runtime>(c: &Case<'_, R>) -> Outcome {
    let sb = step("create", c.runtime().create(spec(c, &c.sandbox("a")?))).await?;
    let out = sh(&sb, "kill -9 $$").await?;
    check!(
        !out.status.success(),
        "a signal-killed command reported success (code {})",
        out.status.code
    );
    Ok(())
}

async fn exec_timeout_is_enforced<R: Runtime>(c: &Case<'_, R>) -> Outcome {
    let sb = step("create", c.runtime().create(spec(c, &c.sandbox("a")?))).await?;
    let started = Instant::now();
    let request = ExecRequest::new("sleep", ["30"])
        .as_user("root")
        .with_timeout(Duration::from_secs(2));
    let err = fails("sleep 30 with a 2 s timeout", sb.exec(request)).await?;
    check!(
        matches!(err, ComputeError::ExecTimeout { .. }),
        "want ExecTimeout, got {err:?}"
    );
    check!(
        started.elapsed() < Duration::from_secs(20),
        "the timeout took {:?}",
        started.elapsed()
    );
    let out = sh(&sb, "exit 0").await?;
    check!(out.status.success(), "sandbox unusable after a timeout");
    Ok(())
}

async fn exec_needs_a_running_sandbox<R: Runtime>(c: &Case<'_, R>) -> Outcome {
    let sb = step("create", c.runtime().create(spec(c, &c.sandbox("a")?))).await?;
    step("stop", sb.stop()).await?;
    let err = fails(
        "exec on a stopped sandbox",
        sb.exec(ExecRequest::sh("exit 0")),
    )
    .await?;
    check!(
        matches!(
            err,
            ComputeError::InvalidState { .. } | ComputeError::StaleHandle { .. }
        ),
        "want InvalidState, got {err:?}"
    );
    Ok(())
}

async fn create_env_reaches_exec<R: Runtime>(c: &Case<'_, R>) -> Outcome {
    let mut create_env = GuestEnv::new();
    create_env
        .set("PUDDLE_CT", "from-create")
        .map_err(|e| e.to_string())?;
    let s = spec(c, &c.sandbox("a")?).with_env(&create_env);
    let sb = step("create", c.runtime().create(s)).await?;
    let out = step(
        "printenv",
        sb.exec(ExecRequest::new("printenv", ["PUDDLE_CT"])),
    )
    .await?;
    check!(
        out.stdout_text() == "from-create\n",
        "create-time env: got {:?}",
        out.stdout_text()
    );
    let mut exec_env = GuestEnv::new();
    exec_env
        .set("PUDDLE_CT_EXEC", "from-exec")
        .map_err(|e| e.to_string())?;
    let out = step(
        "printenv",
        sb.exec(ExecRequest::new("printenv", ["PUDDLE_CT_EXEC"]).with_env(&exec_env)),
    )
    .await?;
    check!(
        out.stdout_text() == "from-exec\n",
        "per-exec env: got {:?}",
        out.stdout_text()
    );
    Ok(())
}

async fn stdin_reaches_exec<R: Runtime>(c: &Case<'_, R>) -> Outcome {
    let sb = step("create", c.runtime().create(spec(c, &c.sandbox("a")?))).await?;
    let out = step(
        "cat",
        sb.exec(ExecRequest::new("cat", Vec::<String>::new()).with_stdin("hello\n")),
    )
    .await?;
    check!(
        out.stdout == b"hello\n",
        "cat echoed {:?}",
        out.stdout_text()
    );
    Ok(())
}

async fn file_mount_is_read_only<R: Runtime>(c: &Case<'_, R>) -> Outcome {
    let host = c.host_file("mounted", b"mounted-ok\n")?;
    let s = spec(c, &c.sandbox("a")?)
        .with_file_mount(FileMount::read_only(host, path("/puddle-ct/file")?));
    let sb = step("create", c.runtime().create(s)).await?;
    let content = read_file(&sb, "/puddle-ct/file").await?;
    check!(
        content.as_deref() == Some("mounted-ok\n"),
        "mounted file reads {content:?}"
    );
    let out = step(
        "write the mounted file",
        sb.exec(
            ExecRequest::new("tee", ["/puddle-ct/file"])
                .with_stdin("x")
                .as_user("root"),
        ),
    )
    .await?;
    check!(!out.status.success(), "writing a read-only mount succeeded");
    check!(
        out.stderr_text().contains("Read-only file system"),
        "want EROFS, got {:?}",
        out.stderr_text()
    );
    Ok(())
}

async fn dir_mount_is_read_only<R: Runtime>(c: &Case<'_, R>) -> Outcome {
    let host = c.host_tree("tree", &[("top", b"top-ok\n"), ("sub/deep", b"deep-ok\n")])?;
    let s = spec(c, &c.sandbox("a")?)
        .with_file_mount(FileMount::read_only_dir(host, path("/puddle-ct/tree")?));
    let sb = step("create", c.runtime().create(s)).await?;
    for (file, want) in [
        ("/puddle-ct/tree/top", "top-ok\n"),
        ("/puddle-ct/tree/sub/deep", "deep-ok\n"),
    ] {
        let got = read_file(&sb, file).await?;
        check!(got.as_deref() == Some(want), "{file} reads {got:?}");
    }
    let missing = read_file(&sb, "/puddle-ct/tree/absent").await?;
    check!(missing.is_none(), "an absent file read as {missing:?}");
    for target in [
        "/puddle-ct/tree/top",
        "/puddle-ct/tree/sub/deep",
        "/puddle-ct/tree/new",
        "/puddle-ct/tree/sub/new",
    ] {
        let out = step(
            "write below the mounted directory",
            sb.exec(
                ExecRequest::new("tee", [target])
                    .with_stdin("x")
                    .as_user("root"),
            ),
        )
        .await?;
        check!(!out.status.success(), "writing {target} succeeded");
        check!(
            out.stderr_text().contains("Read-only file system"),
            "{target}: want EROFS, got {:?}",
            out.stderr_text()
        );
    }
    let still = read_file(&sb, "/puddle-ct/tree/top").await?;
    check!(
        still.as_deref() == Some("top-ok\n"),
        "the mounted file changed to {still:?}"
    );
    Ok(())
}

async fn root_disk_survives_restart_not_remove<R: Runtime>(c: &Case<'_, R>) -> Outcome {
    let rt = c.runtime();
    let name = c.sandbox("a")?;
    let marker = "/root/puddle-ct-marker";
    let sb = step("create", rt.create(spec(c, &name))).await?;
    write_file(&sb, marker, "m1").await?;
    step("stop", sb.stop()).await?;
    let sb = step("start", rt.start(&name)).await?;
    let got = read_file(&sb, marker).await?;
    check!(
        got.as_deref() == Some("m1"),
        "after restart the marker reads {got:?}"
    );
    step("stop", sb.stop()).await?;
    step("remove", rt.remove(&name)).await?;
    let sb = step("create again", rt.create(spec(c, &name))).await?;
    let got = read_file(&sb, marker).await?;
    check!(got.is_none(), "a recreated sandbox has the old marker");
    Ok(())
}

async fn owned_disk_survives_restart_not_remove<R: Runtime>(c: &Case<'_, R>) -> Outcome {
    let rt = c.runtime();
    let name = c.sandbox("a")?;
    let with_disk = || {
        Ok::<_, String>(spec(c, &name).with_owned_disk(OwnedDisk {
            guest: path("/var/lib/puddle-ct")?,
            size: DiskSize::mib(256),
        }))
    };
    let marker = "/var/lib/puddle-ct/marker";
    let sb = step("create", rt.create(with_disk()?)).await?;
    write_file(&sb, marker, "owned").await?;
    step("stop", sb.stop()).await?;
    let sb = step("start", rt.start(&name)).await?;
    let got = read_file(&sb, marker).await?;
    check!(
        got.as_deref() == Some("owned"),
        "after restart the owned disk reads {got:?}"
    );
    step("stop", sb.stop()).await?;
    step("remove", rt.remove(&name)).await?;
    let sb = step("create again", rt.create(with_disk()?)).await?;
    let got = read_file(&sb, marker).await?;
    check!(got.is_none(), "the owned disk outlived its sandbox");
    Ok(())
}

async fn named_volume_survives_restart_and_remove<R: Runtime>(c: &Case<'_, R>) -> Outcome {
    let rt = c.runtime();
    let name = c.sandbox("a")?;
    let vol = c.volume("ws")?;
    let mount = path("/workspace")?;
    let marker = "/workspace/marker";
    let s = spec(c, &name).with_volume(
        VolumeMount::named(vol.clone(), mount.clone()).ensure_size(DiskSize::mib(256)),
    );
    let sb = step("create", rt.create(s)).await?;
    write_file(&sb, marker, "kept").await?;
    step("stop", sb.stop()).await?;
    let sb = step("start", rt.start(&name)).await?;
    let got = read_file(&sb, marker).await?;
    check!(
        got.as_deref() == Some("kept"),
        "after restart the volume reads {got:?}"
    );
    step("stop", sb.stop()).await?;
    step("remove", rt.remove(&name)).await?;
    let info = step("volume", rt.volume(&vol)).await?;
    check!(
        info.as_ref().map(|i| i.size) == Some(DiskSize::mib(256)),
        "after removing the sandbox the volume is {info:?}"
    );
    let plain = spec(c, &name).with_volume(VolumeMount::named(vol, mount));
    let sb = step("create with plain named()", rt.create(plain)).await?;
    let got = read_file(&sb, marker).await?;
    check!(
        got.as_deref() == Some("kept"),
        "after recreate the volume reads {got:?}"
    );
    Ok(())
}

async fn volume_capacity_reads_back<R: Runtime>(c: &Case<'_, R>) -> Outcome {
    let rt = c.runtime();
    let vol = c.volume("ws")?;
    let new = || VolumeSpec {
        name: vol.clone(),
        size: DiskSize::mib(256),
    };
    let info = step("create volume", rt.create_volume(new())).await?;
    check!(
        info.name == vol.as_str(),
        "created volume named {}",
        info.name
    );
    check!(
        info.size == DiskSize::mib(256),
        "created size {}",
        info.size
    );
    let read = step("volume", rt.volume(&vol)).await?;
    check!(
        read.as_ref()
            .is_some_and(|i| i.size == DiskSize::mib(256) && i.holder.is_none()),
        "read back {read:?}"
    );
    let all = step("list volumes", rt.list_volumes()).await?;
    check!(
        all.iter().any(|i| i.name == vol.as_str()),
        "list_volumes misses it"
    );
    let err = fails("create it again", rt.create_volume(new())).await?;
    check!(
        matches!(err, ComputeError::VolumeExists { .. }),
        "want VolumeExists, got {err:?}"
    );
    step("remove volume", rt.remove_volume(&vol)).await?;
    let gone = step("volume", rt.volume(&vol)).await?;
    check!(gone.is_none(), "still there after remove: {gone:?}");
    Ok(())
}

async fn second_attach_is_refused_naming_the_holder<R: Runtime>(c: &Case<'_, R>) -> Outcome {
    let rt = c.runtime();
    let (a, b) = (c.sandbox("first")?, c.sandbox("second")?);
    let vol = c.volume("ws")?;
    let mount = path("/workspace")?;
    let attach = |name: &SandboxName| {
        spec(c, name).with_volume(
            VolumeMount::named(vol.clone(), mount.clone()).ensure_size(DiskSize::mib(256)),
        )
    };
    let first = step("create first", rt.create(attach(&a))).await?;
    let info = step("volume", rt.volume(&vol)).await?;
    check!(
        info.as_ref().and_then(|i| i.holder.as_deref()) == Some(a.as_str()),
        "holder should be {a}: {info:?}"
    );
    let err = fails("create second", rt.create(attach(&b))).await?;
    check!(
        matches!(err, ComputeError::VolumeInUse { ref holder, .. } if holder == a.as_str()),
        "want VolumeInUse naming {a}, got {err:?}"
    );
    let err = fails("remove a held volume", rt.remove_volume(&vol)).await?;
    check!(
        matches!(err, ComputeError::VolumeInUse { .. }),
        "want VolumeInUse, got {err:?}"
    );
    let leftover = status_in_list(rt, &b).await?;
    check!(
        leftover.is_none_or(WorkspaceStatus::is_down),
        "the refused sandbox is {leftover:?}"
    );
    step("stop first", first.stop()).await?;
    let info = step("volume", rt.volume(&vol)).await?;
    check!(
        info.as_ref().is_some_and(|i| i.holder.is_none()),
        "holder after stop: {info:?}"
    );
    if leftover.is_some() {
        step("remove the refused sandbox", rt.remove(&b)).await?;
    }
    let _second = step(
        "create second after the first stopped",
        rt.create(attach(&b)),
    )
    .await?;
    Ok(())
}

async fn failed_create_leaves_no_stale_dir<R: Runtime>(c: &Case<'_, R>) -> Outcome {
    let rt = c.runtime();
    let name = c.sandbox("a")?;
    let vol = c.volume("ws")?;
    let mount = path("/workspace")?;
    step(
        "create volume",
        rt.create_volume(VolumeSpec {
            name: vol.clone(),
            size: DiskSize::mib(256),
        }),
    )
    .await?;
    let sized = |mib| {
        spec(c, &name).with_volume(
            VolumeMount::named(vol.clone(), mount.clone()).ensure_size(DiskSize::mib(mib)),
        )
    };
    let err = fails("create with the wrong size", rt.create(sized(128))).await?;
    check!(
        matches!(
            err,
            ComputeError::VolumeSizeMismatch {
                existing_mib: 256,
                requested_mib: 128,
                ..
            }
        ),
        "want VolumeSizeMismatch 256/128, got {err:?}"
    );
    check!(
        status_in_list(rt, &name).await?.is_none(),
        "the failed sandbox is listed"
    );
    let stale = step("stale dirs", rt.stale_dirs())
        .await?
        .iter()
        .any(|d| d == name.as_str());
    check!(!stale, "a stale directory is left for {name}");
    let _sb = step("create with the right size", rt.create(sized(256))).await?;
    Ok(())
}

async fn missing_volume_fails_create<R: Runtime>(c: &Case<'_, R>) -> Outcome {
    let rt = c.runtime();
    let vol = c.volume("missing")?;
    let s =
        spec(c, &c.sandbox("a")?).with_volume(VolumeMount::named(vol.clone(), path("/workspace")?));
    let err = fails("create with a missing volume", rt.create(s)).await?;
    check!(
        matches!(err, ComputeError::VolumeNotFound { ref volume } if volume == vol.as_str()),
        "want VolumeNotFound, got {err:?}"
    );
    let info = step("volume", rt.volume(&vol)).await?;
    check!(
        info.is_none(),
        "the volume was created implicitly: {info:?}"
    );
    Ok(())
}
