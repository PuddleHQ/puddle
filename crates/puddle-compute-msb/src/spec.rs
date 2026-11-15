// SPDX-License-Identifier: AGPL-3.0-or-later
//! [`puddle_compute::SandboxSpec`] to the SDK's `SandboxBuilder`, and the list of every SDK option
//! Puddle sets or leaves at its default ([`SDK_OPTIONS`], input for the escape-hardening review).

use microsandbox::Sandbox;
use microsandbox::sandbox::{
    DeploymentProfile, PullPolicy, RlimitResource, SandboxBuilder, SecurityProfile,
};
use puddle_compute::{NetworkPolicy, SandboxSpec};

use crate::{OWNER_LABEL, OWNER_LABEL_VALUE};

/// Whether Puddle sets an SDK option, and to what.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SdkSetting {
    /// Puddle sets it, to this.
    Set(&'static str),
    /// Puddle leaves it; this is the SDK's default (msb 0.7.7).
    Default(&'static str),
}

/// One SDK option and what Puddle does with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SdkOption {
    /// Where the option lives: `SandboxBuilder::<method>`, `MountBuilder::…`, `exec`, `ssh`,
    /// `LocalBackend`, `config.json`.
    pub option: &'static str,
    /// What Puddle does.
    pub setting: SdkSetting,
    /// Why.
    pub why: &'static str,
}

const fn set(option: &'static str, value: &'static str, why: &'static str) -> SdkOption {
    SdkOption {
        option,
        setting: SdkSetting::Set(value),
        why,
    }
}

const fn default(option: &'static str, value: &'static str, why: &'static str) -> SdkOption {
    SdkOption {
        option,
        setting: SdkSetting::Default(value),
        why,
    }
}

/// Soft open-file limit every guest process starts with: enough for a package manager or a
/// browser, and small enough that tools which close every descriptor at start stay fast.
pub const GUEST_OPEN_FILES_SOFT: u64 = 65_536;

/// Hard open-file limit in the guest; a process may raise its soft limit up to it.
pub const GUEST_OPEN_FILES_HARD: u64 = 1_048_576;

/// Every `SandboxBuilder` option of SDK 0.7.7 (and the mount, exec, SSH and backend options the
/// adapter touches), with what Puddle does. `SandboxBuilder` entries are kept in step with
/// [`builder`] by a unit test: every method it calls is listed as [`SdkSetting::Set`] and
/// nothing else is.
pub const SDK_OPTIONS: &[SdkOption] = &[
    // SandboxBuilder: set
    set(
        "SandboxBuilder::image",
        "spec image (OCI reference)",
        "the workspace image",
    ),
    set(
        "SandboxBuilder::memory",
        "spec memory (MiB, default 8 GiB)",
        "the memory setting, applied at create; changes apply at the next start via modify().memory().next_start()",
    ),
    set(
        "SandboxBuilder::cpus",
        "spec cpus, only when given (the workspace's cpus setting, applied at create)",
        "otherwise msb's default (1 vCPU); changes apply at the next start via modify().cpus().next_start()",
    ),
    set(
        "SandboxBuilder::root_disk",
        "spec root disk size (MiB), only when given (a workspace's sandbox: 16 GiB)",
        "a managed sparse ext4 upper that takes host space only as the guest writes; otherwise msb's default of 4 096 MiB (maintenance sandboxes)",
    ),
    set(
        "SandboxBuilder::env",
        "spec env (proxy vars, git identity, ...)",
        "reaches exec, SSH and the boot hook",
    ),
    set(
        "SandboxBuilder::label",
        "dev.puddle.owner=puddle",
        "reconcile touches only Puddle's sandboxes",
    ),
    set(
        "SandboxBuilder::disable_network",
        "no network device; policy none",
        "the guest's only way out is the vsock route",
    ),
    set(
        "SandboxBuilder::vsock",
        "one stream route per spec route (port -> named pipe / Unix socket)",
        "only routed ports reach the host",
    ),
    set(
        "SandboxBuilder::volume",
        "file and directory mounts: bind + readonly; named volumes: named / named_with(ensure_exists, disk, size); owned disks: owned_with(disk, size)",
        "read-only mounts only; sources under the guest-share root, or a checkout's `.git` that passed the checkout rule",
    ),
    set(
        "SandboxBuilder::security",
        "SecurityProfile::Default (guest root keeps CAP_SYS_ADMIN, no no_new_privs)",
        "root in the VM is intended; Restricted breaks sudo and Docker-in-Docker",
    ),
    set(
        "SandboxBuilder::deployment_profile",
        "DeploymentProfile::SingleTenant",
        "one user's machine; MultiTenant is for shared hosting",
    ),
    set(
        "SandboxBuilder::pull_policy",
        "PullPolicy::IfMissing",
        "the image was pulled just before by pull_image",
    ),
    set(
        "SandboxBuilder::ephemeral",
        "false",
        "the root disk survives stop/start; removal is explicit",
    ),
    set(
        "SandboxBuilder::rlimit_range",
        "RLIMIT_NOFILE soft 65 536, hard 1 048 576",
        "every guest process inherits it, so the user's own tools get the headroom the agent has",
    ),
    // SandboxBuilder: left at the default
    default(
        "SandboxBuilder::max_memory",
        "unset: the SDK makes it equal to memory (no hotplug reserve)",
        "never above memory; set_memory pins it to the new size via modify(); VM test checks the stored config",
    ),
    default(
        "SandboxBuilder::max_cpus",
        "unset: the SDK makes it equal to cpus (no hotplug reserve)",
        "never above cpus; set_cpus pins it to the new count via modify()",
    ),
    default(
        "SandboxBuilder::cpu_placement / placement_profile",
        "unset",
        "no pinning on a workstation",
    ),
    default(
        "SandboxBuilder::root_disk_with / oci_upper_size",
        "msb default writable upper",
        "both are other spellings of root_disk",
    ),
    default("SandboxBuilder::thp", "Madvise", "msb default"),
    default(
        "SandboxBuilder::guest_clock",
        "Sync",
        "guest clock follows the host",
    ),
    default(
        "SandboxBuilder::log_level / quiet_logs",
        "global default",
        "msb logs under the private home",
    ),
    default("SandboxBuilder::detached", "false", "VMs die with Puddle"),
    default(
        "SandboxBuilder::disable_metrics_sample / metrics_sample_interval",
        "msb default sampling",
        "not used yet",
    ),
    default(
        "SandboxBuilder::workdir",
        "image WORKDIR",
        "the boot hook and exec choose their own",
    ),
    default(
        "SandboxBuilder::shell",
        "/bin/sh",
        "the boot hook needs a POSIX sh",
    ),
    default(
        "SandboxBuilder::registry",
        "anonymous, system roots",
        "the adapter pulls first (pull_image), so create finds the image cached",
    ),
    set(
        "RegistryBuilder::extra_ca_certs (pull_image)",
        "msb's configured roots + MsbConfig::registry_roots",
        "corporate roots, so pulls work behind a TLS-intercepting proxy",
    ),
    set(
        "LocalBackendBuilder::registry_proxy (RegistryBuilder::proxy in pull_image)",
        "Puddle's image-pull proxy with a per-run token (MsbConfig::registry_proxy)",
        "pulls take Puddle's way out; an explicit client setting, so the token is not in the process environment and the user's proxy variables are not read",
    ),
    default("SandboxBuilder::slug", "unset", "cloud only"),
    default(
        "SandboxBuilder::replace / replace_with_timeout",
        "off",
        "a taken name is refused (AlreadyExists), never replaced",
    ),
    default(
        "SandboxBuilder::entrypoint / cmd",
        "image's, not run",
        "SDK create/start run no image workload; the boot hook chains the ENTRYPOINT",
    ),
    default(
        "SandboxBuilder::foreground_command / background_command",
        "unset",
        "no workload at create",
    ),
    default(
        "SandboxBuilder::init / init_with",
        "msb's init",
        "msb's own init is used: the init handoff raced and hung stop on msb 0.7.6",
    ),
    default("SandboxBuilder::hostname", "msb default", "not used yet"),
    default(
        "SandboxBuilder::user",
        "image USER",
        "Puddle runs its exec calls as root explicitly",
    ),
    default(
        "SandboxBuilder::network / proxy / prepend_network_policy_rules",
        "unset (network disabled)",
        "Puddle's own proxy is the chokepoint",
    ),
    default(
        "SandboxBuilder::port / port_bind / port_udp / port_udp_bind",
        "none",
        "no published ports; forwards go through Puddle",
    ),
    default(
        "SandboxBuilder::vsock_dgram / vsock_route",
        "none",
        "stream routes only",
    ),
    default(
        "SandboxBuilder::secret / secret_entry / secret_env / secret_violation_action",
        "none",
        "credentials are injected by Puddle's proxy",
    ),
    default(
        "SandboxBuilder::envs / labels",
        "unset",
        "env and label are set one by one",
    ),
    default(
        "SandboxBuilder::script / scripts / config_scripts",
        "none",
        "the boot hook is a mounted file",
    ),
    default(
        "SandboxBuilder::max_duration / idle_timeout",
        "none",
        "sandboxes live until stopped",
    ),
    default(
        "SandboxBuilder::patch / add_patch",
        "none",
        "guest files are written by the boot hook",
    ),
    default(
        "SandboxBuilder::overlay / from_spec_json / override_image / override_snapshot / snapshot_resolved",
        "unset",
        "no snapshots or overlays",
    ),
    default(
        "SandboxBuilder::image_with / add_volume_mount",
        "unset",
        "plain image reference; mounts via volume()",
    ),
    default(
        "MountBuilder::noexec / nosuid / nodev",
        "off",
        "the agent binary runs from its mount; root in the guest anyway",
    ),
    default(
        "MountBuilder::stat_virtualization / host_permissions / owner",
        "Strict / Private / none",
        "msb defaults for virtiofs",
    ),
    default(
        "MountBuilder::follow_root_symlinks",
        "false",
        "a planted symlink can't redirect a mount; sources are also resolved by Puddle",
    ),
    // exec, SSH, backend, config.json
    set(
        "exec",
        "args, env, cwd, user, stdin bytes, timeout from ExecRequest; tty off",
        "one call per ExecRequest",
    ),
    set(
        "ssh::server_with",
        "authorized keys from MsbConfig; inactivity timeout off unless configured",
        "IDE sessions sit idle",
    ),
    default(
        "ssh::host_key",
        "per-sandbox ed25519 key under the sandbox directory, created on first use",
        "msb default",
    ),
    default(
        "ssh::sftp / user",
        "sftp on; guest user msb default",
        "scp/sftp for the IDE",
    ),
    set(
        "LocalBackend::home / config_path",
        "Puddle's private msb home and its config.json",
        "never the user's ~/.microsandbox",
    ),
    default(
        "LocalBackend::default_cpus / default_memory_mib / ca_certs / registry_hosts / ssh_inactivity_timeout_secs / deployment_profile",
        "msb defaults",
        "every sandbox sets what it needs",
    ),
    set(
        "config.json",
        "version 1 + paths.msb + paths.libkrunfw only",
        "no other global defaults can creep in",
    ),
];

/// The SDK builder for `spec`: every option Puddle sets, nothing else (see [`SDK_OPTIONS`]).
/// File mount sources must already have passed [`crate::MsbConfig::check_mount_source`]; every
/// one of them, a checkout's `.git` included, is bound read-only.
#[must_use]
pub fn builder(spec: &SandboxSpec) -> SandboxBuilder {
    let mut b = Sandbox::builder(spec.name.as_str())
        .image(spec.image.as_str())
        .memory(spec.memory.get())
        .label(OWNER_LABEL, OWNER_LABEL_VALUE)
        .security(SecurityProfile::Default)
        .deployment_profile(DeploymentProfile::SingleTenant)
        .pull_policy(PullPolicy::IfMissing)
        .ephemeral(false)
        .rlimit_range(
            RlimitResource::Nofile,
            GUEST_OPEN_FILES_SOFT,
            GUEST_OPEN_FILES_HARD,
        );
    if let Some(cpus) = spec.cpus {
        b = b.cpus(cpus);
    }
    for (name, value) in spec.env.iter() {
        b = b.env(name, value);
    }
    // `NetworkPolicy::None` is the only policy; a variant added later stays disabled here
    // (fail closed) until it is mapped on purpose.
    debug_assert_eq!(spec.network, NetworkPolicy::None);
    b = b.disable_network();
    for route in &spec.routes {
        b = b.vsock(&route.host, route.guest_port);
    }
    if let Some(size) = spec.root_disk {
        b = b.root_disk(size.as_mib());
    }
    for mount in &spec.file_mounts {
        b = b.volume(mount.guest.as_str(), |m| m.bind(&mount.host).readonly());
    }
    for mount in &spec.volumes {
        let volume = mount.volume.as_str();
        b = b.volume(mount.guest.as_str(), |m| match mount.ensure_size {
            None => m.named(volume),
            Some(size) => m.named_with(volume, |v| v.ensure_exists().disk().size(size.as_mib())),
        });
    }
    for disk in &spec.owned_disks {
        b = b.volume(disk.guest.as_str(), |m| {
            m.owned_with(|v| v.disk().size(disk.size.as_mib()))
        });
    }
    b
}

#[cfg(test)]
mod tests;
