// SPDX-License-Identifier: AGPL-3.0-or-later
//! What a sandbox is made of: [`SandboxSpec`] and its parts.

use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};

use puddle_types::{GuestEnv, GuestPath, ImageRef, MemoryMib, SandboxName, VolumeName};

use crate::ComputeError;

/// The guest's network. Puddle only ever uses [`NetworkPolicy::None`]: the guest has no network
/// interface at all, and its only way out is the vsock routes in [`SandboxSpec::routes`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum NetworkPolicy {
    /// No guest network (msb `NetworkPolicy::none()`, CLI `--no-net`).
    #[default]
    None,
}

impl NetworkPolicy {
    /// The only policy Puddle uses: no network.
    #[must_use]
    pub fn none() -> Self {
        Self::None
    }
}

/// A vsock port in the guest that the runtime connects to a host endpoint: a named pipe on
/// Windows (`\\.\pipe\…`), a Unix socket elsewhere. Only routed ports are reachable from the
/// guest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VsockRoute {
    /// The guest-side vsock port (non-zero).
    pub guest_port: u32,
    /// The host endpoint the runtime connects each guest connection to.
    pub host: PathBuf,
}

impl VsockRoute {
    /// A route from guest vsock port `guest_port` to the host endpoint `host`.
    #[must_use]
    pub fn new(guest_port: u32, host: impl Into<PathBuf>) -> Self {
        Self {
            guest_port,
            host: host.into(),
        }
    }
}

/// What a [`FileMount`]'s host path is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MountKind {
    /// One host file appears at the guest path.
    File,
    /// A host directory appears at the guest path with its whole tree.
    Dir,
    /// The `.git` directory of a checkout on the host, which the user picked, appears at the
    /// guest path with its whole tree. The one mount whose source is not a file Puddle wrote for
    /// guests: a runtime accepts it only from a canonical path to a real git directory outside
    /// Puddle's own folders, and only in a sandbox with no route out (see
    /// [`SandboxSpec::validate`]), so the guest that reads it cannot send it anywhere.
    Checkout,
}

/// A host file or directory mounted **read-only** at a guest path (virtiofs; a guest write gets
/// `EROFS`, also for new files in a directory mount). Puddle uses files for the agent binary,
/// the boot hook and its inputs, directories for shared caches and the dev CA, and a checkout's
/// `.git` to clone a workspace's repository from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileMount {
    /// The file or directory on the host.
    pub host: PathBuf,
    /// Where it appears in the guest.
    pub guest: GuestPath,
    /// Whether `host` is a file or a directory.
    pub kind: MountKind,
}

impl FileMount {
    /// Mounts host file `host` read-only at `guest`.
    #[must_use]
    pub fn read_only(host: impl Into<PathBuf>, guest: GuestPath) -> Self {
        Self {
            host: host.into(),
            guest,
            kind: MountKind::File,
        }
    }

    /// Mounts host directory `host` read-only at `guest`: the guest sees its whole tree and
    /// can change none of it.
    #[must_use]
    pub fn read_only_dir(host: impl Into<PathBuf>, guest: GuestPath) -> Self {
        Self {
            host: host.into(),
            guest,
            kind: MountKind::Dir,
        }
    }

    /// Mounts the checkout `.git` directory `git_dir` read-only at `guest` (see
    /// [`MountKind::Checkout`]).
    #[must_use]
    pub fn checkout(git_dir: impl Into<PathBuf>, guest: GuestPath) -> Self {
        Self {
            host: git_dir.into(),
            guest,
            kind: MountKind::Checkout,
        }
    }
}

/// Checks that `git_dir` has the shape of a checkout's git directory, by what is on disk and
/// without following a link: a real directory named `.git` (a `.git` file, as in a linked
/// worktree or a submodule, is refused with that reason) holding a `HEAD` file and `objects` and
/// `refs` directories, and no `commondir` (a linked worktree's). Every runtime applies it to a
/// [`MountKind::Checkout`] at create; a runtime adds its own rules on where the path may be.
///
/// # Errors
///
/// What is wrong, worded to follow `the checkout <path>`.
pub fn check_checkout_dir(git_dir: &Path) -> Result<(), String> {
    if git_dir.file_name().is_none_or(|n| n != ".git") {
        return Err("is not a `.git` directory".into());
    }
    let meta = std::fs::symlink_metadata(git_dir).map_err(|e| format!("can't be read: {e}"))?;
    if meta.is_file() {
        return Err(
            "is a file that points elsewhere, as in a linked worktree or a submodule; \
             start from the main checkout"
                .into(),
        );
    }
    if meta.file_type().is_symlink() {
        return Err("is a link".into());
    }
    if !meta.is_dir() {
        return Err("is not a directory".into());
    }
    for (entry, dir) in [("HEAD", false), ("objects", true), ("refs", true)] {
        let kind = if dir { "directory" } else { "file" };
        match std::fs::symlink_metadata(git_dir.join(entry)) {
            Ok(m) if m.file_type().is_symlink() => {
                return Err(format!("is not a git directory: its {entry} is a link"));
            }
            Ok(m) if (dir && m.is_dir()) || (!dir && m.is_file()) => {}
            _ => return Err(format!("is not a git directory: it has no {entry} {kind}")),
        }
    }
    if std::fs::symlink_metadata(git_dir.join("commondir")).is_ok() {
        return Err(
            "belongs to a linked worktree (it has a commondir file); start from the main checkout"
                .into(),
        );
    }
    Ok(())
}

/// A disk size in MiB (what msb's `.disk().size(n)` takes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DiskSize(u32);

impl DiskSize {
    /// `mib` MiB.
    #[must_use]
    pub const fn mib(mib: u32) -> Self {
        Self(mib)
    }

    /// `gib` GiB (saturating at `u32::MAX` MiB).
    #[must_use]
    pub const fn gib(gib: u32) -> Self {
        Self(gib.saturating_mul(1024))
    }

    /// The size in MiB.
    #[must_use]
    pub const fn as_mib(self) -> u32 {
        self.0
    }

    /// The size in bytes.
    #[must_use]
    pub const fn as_bytes(self) -> u64 {
        // Lossless widening; `u64::from` isn't const.
        (self.0 as u64) * 1024 * 1024
    }
}

impl fmt::Display for DiskSize {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} MiB", self.0)
    }
}

/// A named disk volume attached at a guest path. The volume outlives the sandbox (ADR 0006).
///
/// Without [`VolumeMount::ensure_size`] the volume must exist and is reattached as it is (msb
/// reads kind and size from its catalog). With it, a missing volume is created with that
/// size, and an existing one must have exactly that size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeMount {
    /// The volume.
    pub volume: VolumeName,
    /// Where it is mounted in the guest.
    pub guest: GuestPath,
    /// Create-if-missing with this size; `None` = must already exist.
    pub ensure_size: Option<DiskSize>,
}

impl VolumeMount {
    /// Attaches existing volume `volume` at `guest` (msb `.named(volume)`).
    #[must_use]
    pub fn named(volume: VolumeName, guest: GuestPath) -> Self {
        Self {
            volume,
            guest,
            ensure_size: None,
        }
    }

    /// The same mount, creating the volume with `size` if it doesn't exist (msb
    /// `named_with(.., ensure_exists().disk().size(n))`).
    #[must_use]
    pub fn ensure_size(mut self, size: DiskSize) -> Self {
        self.ensure_size = Some(size);
        self
    }
}

/// A disk that belongs to the sandbox: it survives stop/start and is deleted with the sandbox
/// (msb `owned_with(|v| v.disk().size(n))`). For data that must not sit on the root overlay, such
/// as `/var/lib/docker`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnedDisk {
    /// Where it is mounted in the guest.
    pub guest: GuestPath,
    /// Its size.
    pub size: DiskSize,
}

/// Everything needed to create a sandbox. Build it with [`SandboxSpec::new`] and the `with_*`
/// methods; the fields are public so runtimes can read them.
///
/// ```
/// use puddle_compute::{FileMount, SandboxSpec, VolumeMount, VsockRoute, DiskSize};
/// use puddle_types::{GuestPath, ImageRef, SandboxName, WorkspaceId};
///
/// let ws = WorkspaceId::new("acme").unwrap();
/// let spec = SandboxSpec::new(
///     SandboxName::new("acme").unwrap(),
///     ImageRef::new("mcr.microsoft.com/devcontainers/base:debian").unwrap(),
/// )
/// .with_route(VsockRoute::new(5000, r"\\.\pipe\puddle-acme-proxy"))
/// .with_file_mount(FileMount::read_only("/opt/puddle/boot.sh", GuestPath::new("/puddle/boot.sh").unwrap()))
/// .with_volume(
///     VolumeMount::named(ws.volume_name(), GuestPath::new("/workspaces/acme").unwrap())
///         .ensure_size(DiskSize::gib(20)),
/// );
/// spec.validate().unwrap();
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxSpec {
    /// The sandbox's name.
    pub name: SandboxName,
    /// The OCI image.
    pub image: ImageRef,
    /// Guest memory (`--memory`; Puddle never sets a max-memory).
    pub memory: MemoryMib,
    /// Virtual CPUs; `None` = the runtime's default.
    pub cpus: Option<u8>,
    /// Environment for every process: exec, SSH sessions and the boot hook.
    pub env: GuestEnv,
    /// The guest network: always none.
    pub network: NetworkPolicy,
    /// vsock routes to host endpoints.
    pub routes: Vec<VsockRoute>,
    /// Read-only host file mounts.
    pub file_mounts: Vec<FileMount>,
    /// Named disk volumes.
    pub volumes: Vec<VolumeMount>,
    /// Disks owned by the sandbox.
    pub owned_disks: Vec<OwnedDisk>,
    /// The writable root disk's size; `None` = the runtime's default.
    pub root_disk: Option<DiskSize>,
}

impl SandboxSpec {
    /// A sandbox `name` from `image` with default memory, no network and nothing attached.
    #[must_use]
    pub fn new(name: SandboxName, image: ImageRef) -> Self {
        Self {
            name,
            image,
            memory: MemoryMib::DEFAULT,
            cpus: None,
            env: GuestEnv::new(),
            network: NetworkPolicy::none(),
            routes: Vec::new(),
            file_mounts: Vec::new(),
            volumes: Vec::new(),
            owned_disks: Vec::new(),
            root_disk: None,
        }
    }

    /// Sets the size of the writable root disk. The disk is sparse on the host, so the size is a
    /// limit for the guest and costs host space only as the guest writes.
    #[must_use]
    pub fn with_root_disk(mut self, size: DiskSize) -> Self {
        self.root_disk = Some(size);
        self
    }

    /// Sets the guest memory.
    #[must_use]
    pub fn with_memory(mut self, memory: MemoryMib) -> Self {
        self.memory = memory;
        self
    }

    /// Sets the number of virtual CPUs.
    #[must_use]
    pub fn with_cpus(mut self, cpus: u8) -> Self {
        self.cpus = Some(cpus);
        self
    }

    /// Adds environment variables (later ones win).
    #[must_use]
    pub fn with_env(mut self, env: &GuestEnv) -> Self {
        self.env.extend(env);
        self
    }

    /// Adds a vsock route.
    #[must_use]
    pub fn with_route(mut self, route: VsockRoute) -> Self {
        self.routes.push(route);
        self
    }

    /// Adds a read-only file mount.
    #[must_use]
    pub fn with_file_mount(mut self, mount: FileMount) -> Self {
        self.file_mounts.push(mount);
        self
    }

    /// Adds a named volume.
    #[must_use]
    pub fn with_volume(mut self, mount: VolumeMount) -> Self {
        self.volumes.push(mount);
        self
    }

    /// Adds an owned disk.
    #[must_use]
    pub fn with_owned_disk(mut self, disk: OwnedDisk) -> Self {
        self.owned_disks.push(disk);
        self
    }

    /// Checks what the types can't: routes use distinct non-zero ports, no two mounts share a
    /// guest path or nest, a volume is attached once, CPUs and disk sizes are non-zero, and a
    /// sandbox that mounts a checkout has no route (its guest has no way out).
    /// Runtimes call this at the start of [`crate::Runtime::create`].
    ///
    /// # Errors
    ///
    /// [`ComputeError::InvalidSpec`] naming the first problem found.
    pub fn validate(&self) -> Result<(), ComputeError> {
        let invalid = |reason: String| Err(ComputeError::InvalidSpec { reason });
        if self.cpus == Some(0) {
            return invalid("cpus must be at least 1".into());
        }
        if !self.routes.is_empty()
            && let Some(m) = self
                .file_mounts
                .iter()
                .find(|m| m.kind == MountKind::Checkout)
        {
            return invalid(format!(
                "the checkout mounted at {} may only go to a sandbox with no route",
                m.guest
            ));
        }
        let mut ports = BTreeSet::new();
        for route in &self.routes {
            if route.guest_port == 0 {
                return invalid("vsock route port must not be 0".into());
            }
            if !ports.insert(route.guest_port) {
                return invalid(format!("vsock port {} is routed twice", route.guest_port));
            }
        }
        let mut volumes = BTreeSet::new();
        for mount in &self.volumes {
            if !volumes.insert(&mount.volume) {
                return invalid(format!(
                    "volume {:?} is attached twice",
                    mount.volume.as_str()
                ));
            }
            if mount.ensure_size.is_some_and(|s| s.as_mib() == 0) {
                return invalid(format!("volume {:?} has size 0", mount.volume.as_str()));
            }
        }
        if self.root_disk.is_some_and(|s| s.as_mib() == 0) {
            return invalid("the root disk has size 0".into());
        }
        if let Some(d) = self.owned_disks.iter().find(|d| d.size.as_mib() == 0) {
            return invalid(format!("owned disk at {} has size 0", d.guest));
        }
        let guests: Vec<&GuestPath> = self
            .file_mounts
            .iter()
            .map(|m| &m.guest)
            .chain(self.volumes.iter().map(|m| &m.guest))
            .chain(self.owned_disks.iter().map(|d| &d.guest))
            .collect();
        for (i, a) in guests.iter().enumerate() {
            for b in guests.iter().skip(i + 1) {
                if a.is_within(b) || b.is_within(a) {
                    return invalid(format!("mounts at {a} and {b} overlap"));
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
