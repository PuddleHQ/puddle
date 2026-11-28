// SPDX-License-Identifier: AGPL-3.0-or-later
//! The cases about the sandbox's network surface and the settings that apply at the next start:
//! the SSH banner, vsock routes, memory, CPUs and environment.

use super::*;

/// How many connections [`ssh_server_speaks_first`] opens at once on a fresh sandbox.
const FIRST_SSH_CONNECTIONS: usize = 16;

/// Serves one SSH connection on an in-memory stream; returns how serving ended and the first
/// line the server sent.
async fn ssh_banner<S: Sandbox>(sb: &S) -> (Result<(), ComputeError>, Vec<u8>) {
    let (mut client, server) = tokio::io::duplex(64 * 1024);
    let read_banner = async move {
        let mut line = Vec::new();
        let mut byte = [0_u8; 1];
        while line.len() < 255 && !line.ends_with(b"\n") {
            match client.read(&mut byte).await {
                Ok(1) => line.extend_from_slice(&byte),
                _ => break,
            }
        }
        drop(client);
        line
    };
    tokio::join!(sb.serve_ssh(server), read_banner)
}

pub(super) async fn ssh_server_speaks_first<R: Runtime>(c: &Case<'_, R>) -> Outcome {
    // The first connections to a fresh sandbox arrive together (an editor or a script opens
    // several at once): each one is served, none fails while another sets the server up. A
    // few fresh sandboxes, because only a sandbox's first connections can collide.
    for round in ["a", "b", "c"] {
        let sb = std::sync::Arc::new(
            step("create", c.runtime().create(spec(c, &c.sandbox(round)?))).await?,
        );
        let mut all = tokio::task::JoinSet::new();
        for _ in 0..FIRST_SSH_CONNECTIONS {
            let sb = std::sync::Arc::clone(&sb);
            all.spawn(async move { ssh_banner(&*sb).await });
        }
        let Ok(lines) = tokio::time::timeout(Duration::from_secs(30), all.join_all()).await else {
            return Err("SSH server didn't answer and finish within 30 s".into());
        };
        for (i, (served, line)) in lines.iter().enumerate() {
            check!(
                line.starts_with(b"SSH-2.0-"),
                "sandbox {round}, connection {i}: first line {:?}, serving ended with {served:?}",
                String::from_utf8_lossy(line)
            );
        }
        step("stop", sb.stop()).await?;
        let (_client, server) = tokio::io::duplex(1024);
        let err = fails("serve SSH for a stopped sandbox", sb.serve_ssh(server)).await?;
        check!(
            matches!(
                err,
                ComputeError::InvalidState { .. } | ComputeError::StaleHandle { .. }
            ),
            "want InvalidState, got {err:?}"
        );
    }
    Ok(())
}

pub(super) async fn routes_and_no_network_are_accepted<R: Runtime>(c: &Case<'_, R>) -> Outcome {
    let s = spec(c, &c.sandbox("a")?)
        .with_route(VsockRoute::new(5000, c.route_endpoint("proxy")))
        .with_route(VsockRoute::new(5001, c.route_endpoint("ssh")));
    let sb = step("create with routes", c.runtime().create(s)).await?;
    let out = sh(&sb, "exit 0").await?;
    check!(out.status.success(), "exec in a routed sandbox failed");
    Ok(())
}

/// `MemTotal` from the guest's `/proc/meminfo`, in KiB.
async fn mem_total_kib<S: Sandbox>(sb: &S) -> Result<u64, String> {
    let meminfo = read_file(sb, "/proc/meminfo")
        .await?
        .ok_or("can't read /proc/meminfo")?;
    meminfo
        .lines()
        .find_map(|line| line.strip_prefix("MemTotal:"))
        .and_then(|rest| rest.trim().trim_end_matches("kB").trim().parse().ok())
        .ok_or_else(|| format!("no MemTotal in {meminfo:?}"))
}

pub(super) async fn memory_change_applies_at_next_start<R: Runtime>(c: &Case<'_, R>) -> Outcome {
    let rt = c.runtime();
    let name = c.sandbox("a")?;
    let bigger = MemoryMib::new(1024).map_err(|e| e.to_string())?;
    let sb = step("create with 512 MiB", rt.create(spec(c, &name))).await?;
    // msb's guest sees a little more than the configured size (529224 KiB for 512 MiB), so the
    // check is on the difference: +512 MiB, give or take 64 MiB.
    let before = mem_total_kib(&sb).await?;
    step("set memory to 1024 MiB", rt.set_memory(&name, bigger)).await?;
    let running = mem_total_kib(&sb).await?;
    check!(
        running == before,
        "the running sandbox changed size: {before} -> {running} KiB"
    );
    step("stop", sb.stop()).await?;
    let sb = step("start", rt.start(&name)).await?;
    let after = mem_total_kib(&sb).await?;
    check!(
        after.abs_diff(before + 512 * 1024) <= 64 * 1024,
        "after the restart MemTotal is {after} KiB (was {before} KiB at 512 MiB, want +512 MiB)"
    );
    let err = fails(
        "set memory of an unknown sandbox",
        rt.set_memory(&c.sandbox("nope")?, MemoryMib::MIN),
    )
    .await?;
    check!(
        matches!(err, ComputeError::NotFound { .. }),
        "want NotFound, got {err:?}"
    );
    Ok(())
}

/// How many CPUs the guest has online, from `/sys/devices/system/cpu/online` (`0`, `0-3`,
/// `0,2-3`).
async fn online_cpus<S: Sandbox>(sb: &S) -> Result<u32, String> {
    let online = read_file(sb, "/sys/devices/system/cpu/online")
        .await?
        .ok_or("can't read /sys/devices/system/cpu/online")?;
    online
        .trim()
        .split(',')
        .map(|part| match part.split_once('-') {
            Some((first, last)) => first
                .parse::<u32>()
                .and_then(|first| last.parse::<u32>().map(|last| last + 1 - first)),
            None => part.parse::<u32>().map(|_| 1),
        })
        .sum::<Result<u32, _>>()
        .map_err(|e| format!("can't read the CPU range {online:?}: {e}"))
}

pub(super) async fn cpus_change_applies_at_next_start<R: Runtime>(c: &Case<'_, R>) -> Outcome {
    let rt = c.runtime();
    let name = c.sandbox("a")?;
    let two = CpuCount::new(2).map_err(|e| e.to_string())?;
    let sb = step("create with 1 CPU", rt.create(spec(c, &name).with_cpus(1))).await?;
    let before = online_cpus(&sb).await?;
    check!(before == 1, "created with 1 CPU, the guest has {before}");
    step("set cpus to 2", rt.set_cpus(&name, two)).await?;
    let running = online_cpus(&sb).await?;
    check!(
        running == before,
        "the running sandbox changed its CPUs: {before} -> {running}"
    );
    step("stop", sb.stop()).await?;
    let sb = step("start", rt.start(&name)).await?;
    let after = online_cpus(&sb).await?;
    check!(
        after == 2,
        "after the restart the guest has {after} CPUs, want 2"
    );
    let err = fails(
        "set cpus of an unknown sandbox",
        rt.set_cpus(&c.sandbox("nope")?, CpuCount::MIN),
    )
    .await?;
    check!(
        matches!(err, ComputeError::NotFound { .. }),
        "want NotFound, got {err:?}"
    );
    Ok(())
}

/// The four variables the case watches as one guest sees them ("unset" for an absent one), then
/// the root disk marker.
async fn env_and_marker<S: Sandbox>(sb: &S) -> Result<String, String> {
    let mut seen = Vec::new();
    for var in ["KEPT", "CHANGED", "ADDED", "GONE"] {
        let out = step("printenv", sb.exec(ExecRequest::new("printenv", [var]))).await?;
        seen.push(if out.status.success() {
            out.stdout_text().trim_end().to_owned()
        } else {
            "unset".to_owned()
        });
    }
    let marker = read_file(sb, "/root-disk-marker").await?;
    Ok(format!(
        "{} {}",
        seen.join(" "),
        marker.unwrap_or_else(|| "no marker".to_owned()).trim_end()
    ))
}

fn guest_env(pairs: &[(&str, &str)]) -> Result<GuestEnv, String> {
    let mut env = GuestEnv::new();
    for (k, v) in pairs {
        env.set(k, v).map_err(|e| e.to_string())?;
    }
    Ok(env)
}

pub(super) async fn env_change_applies_at_next_start_and_keeps_the_root_disk<R: Runtime>(
    c: &Case<'_, R>,
) -> Outcome {
    let rt = c.runtime();
    let name = c.sandbox("a")?;
    let made_with = guest_env(&[("KEPT", "same"), ("CHANGED", "old"), ("GONE", "1")])?;
    let sb = step("create", rt.create(spec(c, &name).with_env(&made_with))).await?;
    write_file(&sb, "/root-disk-marker", "on-the-root-disk\n").await?;
    let now = guest_env(&[("KEPT", "same"), ("CHANGED", "new"), ("ADDED", "yes")])?;
    step("set env", rt.set_env(&name, &now)).await?;
    let running = env_and_marker(&sb).await?;
    check!(
        running == "same old unset 1 on-the-root-disk",
        "the running sandbox changed its variables: {running:?}"
    );
    step("stop", sb.stop()).await?;
    let sb = step("start", rt.start(&name)).await?;
    let after = env_and_marker(&sb).await?;
    check!(
        after == "same new yes unset on-the-root-disk",
        "after the restart: {after:?}, want the new variables and the root disk file"
    );
    let err = fails(
        "set env of an unknown sandbox",
        rt.set_env(&c.sandbox("nope")?, &now),
    )
    .await?;
    check!(
        matches!(err, ComputeError::NotFound { .. }),
        "want NotFound, got {err:?}"
    );
    Ok(())
}
