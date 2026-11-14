// SPDX-License-Identifier: AGPL-3.0-or-later
//! The guest scripts, embedded in the binary, and the read-only mounts that put them into a
//! sandbox.

use std::io;
use std::path::Path;

use puddle_compute::{FileMount, SandboxSpec};
use puddle_types::guest_path;

/// `guest/boot.sh`: the hook itself.
pub const BOOT_SH: &str = include_str!("../guest/boot.sh");

/// `guest/agent-supervise.sh`: restarts `puddle-agent` whenever it exits.
pub const AGENT_SUPERVISE_SH: &str = include_str!("../guest/agent-supervise.sh");

/// Where `boot.sh` is mounted in the guest.
pub const BOOT_SH_GUEST: &str = "/puddle/boot.sh";

/// Where `agent-supervise.sh` is mounted (`boot.sh` finds it next to itself).
pub const AGENT_SUPERVISE_GUEST: &str = "/puddle/agent-supervise.sh";

/// Where the `puddle-agent` binary is mounted by default (`ci/build-agent.sh` builds it).
pub const AGENT_GUEST: &str = "/puddle/puddle-agent";

/// Where the code-server release is mounted (a read-only directory).
pub const CODE_SERVER_GUEST: &str = "/puddle/code-server";

/// The guest directory with Puddle's read-only mounts. The plan may not write below it.
pub const MOUNT_DIR_GUEST: &str = "/puddle";

/// Writes `boot.sh` and `agent-supervise.sh` into the host directory `dir` (created if needed)
/// and returns the read-only mounts that put them at [`BOOT_SH_GUEST`] and
/// [`AGENT_SUPERVISE_GUEST`]. Add them to the spec with [`with_boot_mounts`].
///
/// # Errors
///
/// When `dir` can't be created or a file can't be written.
pub fn write_assets(dir: &Path) -> io::Result<Vec<FileMount>> {
    std::fs::create_dir_all(dir)?;
    let boot = dir.join("boot.sh");
    let supervise = dir.join("agent-supervise.sh");
    std::fs::write(&boot, BOOT_SH)?;
    std::fs::write(&supervise, AGENT_SUPERVISE_SH)?;
    Ok(vec![
        FileMount::read_only(boot, guest_path!(BOOT_SH_GUEST)),
        FileMount::read_only(supervise, guest_path!(AGENT_SUPERVISE_GUEST)),
    ])
}

/// `spec` with the code-server release folder `dir` mounted read-only at [`CODE_SERVER_GUEST`].
#[must_use]
pub fn with_code_server_mount(spec: SandboxSpec, dir: &Path) -> SandboxSpec {
    spec.with_file_mount(FileMount::read_only_dir(
        dir,
        guest_path!(CODE_SERVER_GUEST),
    ))
}

/// `spec` with the boot scripts in `assets` (from [`write_assets`]) and, if given, the agent
/// binary at [`AGENT_GUEST`] mounted read-only.
#[must_use]
pub fn with_boot_mounts(
    mut spec: SandboxSpec,
    assets: Vec<FileMount>,
    agent_binary: Option<&Path>,
) -> SandboxSpec {
    for m in assets {
        spec = spec.with_file_mount(m);
    }
    if let Some(agent) = agent_binary {
        spec = spec.with_file_mount(FileMount::read_only(agent, guest_path!(AGENT_GUEST)));
    }
    spec
}

#[cfg(test)]
mod tests;
