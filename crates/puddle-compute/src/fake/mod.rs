// SPDX-License-Identifier: AGPL-3.0-or-later
//! [`FakeRuntime`]: an in-memory [`Runtime`] that behaves like the msb Puddle pins, for
//! unit tests of everything above the compute plane (feature `fake`).
//!
//! What it reproduces (each pinned by a case in [`crate::contract`]):
//!
//! - `create` boots the sandbox and returns an owning handle; dropping it without `stop` leaves
//!   the sandbox `Stopped`, as msb 0.7.7 does (0.7.6 said `Crashed`). `get`
//!   re-adopts a running sandbox without owning it.
//! - Exit codes come through exactly; a signal-killed command reports `-1`.
//! - File mounts are read-only (`tee` reports `Read-only file system`).
//! - Root disk and owned disks survive stop/start and die with `remove`; named volumes survive
//!   `remove` and reattach with a plain `named()` mount.
//! - A second attach of a volume held by a running sandbox is refused naming the holder, and
//!   leaves the refused sandbox behind as a `Stopped` record.
//! - A create that fails leaves no stale directory ([`FakeRuntime::add_stale_dir`] plants one, as
//!   a crash would).
//! - The guest has no network, and only the sandbox's routed vsock ports on the host accept (the
//!   hostile-guest probes of [`crate::probes`]).
//! - The SSH server writes an `SSH-2.0-` identification line first.
//! - Every boot writes `/proc/meminfo` with `MemTotal` = the spec's memory, so a
//!   [`Runtime::set_memory`] shows only after the next start.
//! - Every boot writes `/sys/devices/system/cpu/online` as the CPU range the spec's cpus make
//!   (`0` for one, `0-3` for four), so a [`Runtime::set_cpus`] shows only after the next start.
//!
//! Test controls: [`FakeRuntime::inject`] makes the next matching call(s) fail
//! ([`Fault`]), [`FakeRuntime::calls`] returns every call in order, [`FakeRuntime::on_exec`]
//! adds command handlers, [`FakeRuntime::crash`] kills a VM, [`FakeRuntime::add_image`] adds
//! pullable images, [`FakeRuntime::wait_in_every_call`] makes every call wait once.
//!
//! ```
//! # tokio_test_block_on(async {
//! use puddle_compute::fake::{FakeRuntime, Fault, Op};
//! use puddle_compute::{ComputeError, ExecRequest, Runtime, Sandbox, SandboxSpec};
//! use puddle_types::{ImageRef, SandboxName};
//!
//! let rt = FakeRuntime::new();
//! let spec = SandboxSpec::new(SandboxName::new("demo").unwrap(), ImageRef::new(FakeRuntime::DEBIAN).unwrap());
//! let sb = rt.create(spec).await.unwrap();
//! assert_eq!(sb.exec(ExecRequest::sh("exit 3")).await.unwrap().status.code, 3);
//!
//! rt.inject(Op::Stop, Fault::once(ComputeError::Runtime { op: "stop", message: "injected".into() }));
//! assert!(sb.stop().await.is_err());
//! sb.stop().await.unwrap();
//! # });
//! # fn tokio_test_block_on<F: std::future::Future>(f: F) -> F::Output {
//! #     tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(f)
//! # }
//! ```

mod exec;
mod guest_probes;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use puddle_types::{
    CpuCount, GuestEnv, ImageRef, MemoryMib, SandboxName, VolumeName, WorkspaceStatus,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub use self::exec::{ExecContext, ExecHandler, FsError};
use self::exec::{Files, builtin};
use crate::{
    Capabilities, ComputeError, DiskSize, ExecOutput, ExecRequest, ImageConfig, MountKind,
    RootDiskGrowth, RootDiskUse, Runtime, Sandbox, SandboxInfo, SandboxSpec, SshStream, VolumeInfo,
    VolumeSpec,
};

/// The identification line the fake SSH server sends.
pub const SSH_BANNER: &str = "SSH-2.0-puddle_fake\r\n";

/// Every operation of the [`Runtime`] and [`Sandbox`] traits, for [`FakeRuntime::inject`] and
/// [`FakeRuntime::calls`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Op {
    /// [`Runtime::probe`].
    Probe,
    /// [`Runtime::pull_image`].
    PullImage,
    /// [`Runtime::create`].
    Create,
    /// [`Runtime::start`].
    Start,
    /// [`Runtime::get`].
    Get,
    /// [`Runtime::set_memory`].
    SetMemory,
    /// [`Runtime::set_cpus`].
    SetCpus,
    /// [`Runtime::set_env`].
    SetEnv,
    /// [`Runtime::grow_root_disk`].
    GrowRootDisk,
    /// [`Runtime::list`].
    List,
    /// [`Runtime::remove`].
    Remove,
    /// [`Runtime::stale_dirs`].
    StaleDirs,
    /// [`Runtime::remove_stale_dir`].
    RemoveStaleDir,
    /// [`Runtime::create_volume`].
    CreateVolume,
    /// [`Runtime::volume`].
    Volume,
    /// [`Runtime::list_volumes`].
    ListVolumes,
    /// [`Runtime::remove_volume`].
    RemoveVolume,
    /// [`Sandbox::status`].
    Status,
    /// [`Sandbox::stop`].
    Stop,
    /// [`Sandbox::exec`].
    Exec,
    /// [`Sandbox::serve_ssh`].
    ServeSsh,
}

/// One recorded call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Call {
    /// The operation.
    pub op: Op,
    /// The sandbox, volume or image it was about, if any.
    pub target: Option<String>,
    /// For [`Op::Exec`]: the program and its arguments, space-separated.
    pub detail: Option<String>,
}

/// A failure to inject: which error, for which target, how many calls to let through first and
/// how many to fail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fault {
    error: ComputeError,
    target: Option<String>,
    skip: u32,
    remaining: Option<u32>,
}

impl Fault {
    /// Fails the next matching call with `error`.
    #[must_use]
    pub fn once(error: ComputeError) -> Self {
        Self {
            error,
            target: None,
            skip: 0,
            remaining: Some(1),
        }
    }

    /// Fails every matching call with `error` until [`FakeRuntime::clear_faults`].
    #[must_use]
    pub fn always(error: ComputeError) -> Self {
        Self {
            remaining: None,
            ..Self::once(error)
        }
    }

    /// Lets `n` matching calls succeed first.
    #[must_use]
    pub fn after(mut self, n: u32) -> Self {
        self.skip = n;
        self
    }

    /// Fails `n` matching calls (instead of one).
    #[must_use]
    pub fn times(mut self, n: u32) -> Self {
        self.remaining = Some(n);
        self
    }

    /// Only matches calls about this sandbox, volume or image.
    #[must_use]
    pub fn only_for(mut self, target: impl Into<String>) -> Self {
        self.target = Some(target.into());
        self
    }
}

/// What a test can set about the fake itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FakeConfig {
    /// Reported by [`Runtime::probe`].
    pub runtime_version: String,
}

impl Default for FakeConfig {
    fn default() -> Self {
        Self {
            runtime_version: "fake".into(),
        }
    }
}

pub(crate) struct VolumeRecord {
    size: DiskSize,
    /// What the volume "takes on the host", as a test sets it; `None` is a runtime that cannot say.
    used: Option<DiskSize>,
    files: Files,
}

struct SandboxRecord {
    spec: SandboxSpec,
    status: WorkspaceStatus,
    boot: u64,
    files: Files,
    /// What the root disk "holds on the host", as a test sets it; `None` is a runtime that
    /// cannot say.
    root_disk: Option<RootDiskUse>,
    /// A size [`Runtime::grow_root_disk`] left for the next boot (the runtime could not grow
    /// live), applied by [`SandboxRecord::boot`].
    pending_root: Option<DiskSize>,
    /// The variables the guest booted with: a [`Runtime::set_env`] on a running sandbox only
    /// shows at the next boot, as on a real guest.
    booted_env: GuestEnv,
}

impl SandboxRecord {
    /// The reported root disk capacity becomes `size`; the use stays what it was.
    fn grow_reported(&mut self, size: DiskSize) {
        if let Some(root) = &mut self.root_disk {
            root.size = size;
        }
    }

    /// The guest's view of its memory at boot, like the kernel's `/proc/meminfo`.
    fn boot(&mut self, boot: u64) {
        self.status = WorkspaceStatus::Running;
        self.boot = boot;
        self.booted_env = self.spec.env.clone();
        if let Some(size) = self.pending_root.take() {
            self.grow_reported(size);
        }
        let kib = u64::from(self.spec.memory.get()) * 1024;
        self.files.insert(
            MEMINFO.to_owned(),
            format!("MemTotal:       {kib} kB\n").into_bytes(),
        );
        let last = self.spec.cpus.unwrap_or(1).saturating_sub(1);
        let online = if last == 0 {
            "0\n".to_owned()
        } else {
            format!("0-{last}\n")
        };
        self.files
            .insert(CPUS_ONLINE.to_owned(), online.into_bytes());
    }
}

/// Where the fake writes `MemTotal` at every boot.
const MEMINFO: &str = "/proc/meminfo";

/// Where the fake writes the online CPU range at every boot.
const CPUS_ONLINE: &str = "/sys/devices/system/cpu/online";

#[derive(Default)]
struct State {
    images: BTreeMap<String, ImageConfig>,
    sandboxes: BTreeMap<String, SandboxRecord>,
    /// Sandboxes Puddle didn't create ([`FakeRuntime::add_foreign_sandbox`]): listed only.
    foreign: BTreeMap<String, WorkspaceStatus>,
    stale_dirs: BTreeSet<String>,
    volumes: BTreeMap<String, VolumeRecord>,
    faults: Vec<(Op, Fault)>,
    calls: Vec<Call>,
    handlers: Vec<Arc<dyn ExecHandler>>,
    boots: u64,
    /// Sandboxes whose next stop times out gracefully and is ended by force.
    force_stop: BTreeSet<String>,
    /// [`FakeRuntime::serve_terminals`]: `serve_ssh` runs the pseudo-terminal stand-in.
    terminals: bool,
    /// [`FakeRuntime::set_live_root_grow`] turned off: a running sandbox grows at its next boot.
    no_live_root_grow: bool,
    /// [`FakeRuntime::wait_in_every_call`]: each call waits once before it answers.
    waits: bool,
}

impl State {
    /// Records the call, then fires the first matching fault, if any.
    fn enter(
        &mut self,
        op: Op,
        target: Option<&str>,
        detail: Option<String>,
    ) -> Result<(), ComputeError> {
        self.calls.push(Call {
            op,
            target: target.map(str::to_owned),
            detail,
        });
        let hit = self.faults.iter_mut().position(|(fop, f)| {
            *fop == op && (f.target.is_none() || f.target.as_deref() == target)
        });
        let Some(i) = hit else { return Ok(()) };
        let Some((_, fault)) = self.faults.get_mut(i) else {
            return Ok(());
        };
        if fault.skip > 0 {
            fault.skip -= 1;
            return Ok(());
        }
        let error = fault.error.clone();
        match &mut fault.remaining {
            Some(1) => {
                self.faults.remove(i);
            }
            Some(n) => *n -= 1,
            None => {}
        }
        Err(error)
    }

    fn holder_of(&self, volume: &str) -> Option<String> {
        self.sandboxes
            .iter()
            .find(|(_, r)| {
                r.status == WorkspaceStatus::Running
                    && r.spec.volumes.iter().any(|m| m.volume.as_str() == volume)
            })
            .map(|(name, _)| name.clone())
    }

    fn volume_info(&self, name: &str, v: &VolumeRecord) -> VolumeInfo {
        VolumeInfo {
            name: name.to_owned(),
            size: v.size,
            used: v.used,
            holder: self.holder_of(name),
        }
    }

    /// The holder check every boot runs: a volume attached to another running sandbox.
    fn check_holders(&self, spec: &SandboxSpec) -> Result<(), ComputeError> {
        for m in &spec.volumes {
            if let Some(holder) = self.holder_of(m.volume.as_str())
                && holder != spec.name.as_str()
            {
                return Err(ComputeError::VolumeInUse {
                    volume: m.volume.to_string(),
                    holder,
                });
            }
        }
        Ok(())
    }

    fn next_boot(&mut self) -> u64 {
        self.boots += 1;
        self.boots
    }
}

struct Inner {
    config: FakeConfig,
    state: Mutex<State>,
}

/// An in-memory msb. Cloning gives another handle to the same fake, so a test can keep one for
/// [`FakeRuntime::inject`] while code under test owns another.
#[derive(Clone)]
pub struct FakeRuntime {
    inner: Arc<Inner>,
}

impl Default for FakeRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for FakeRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakeRuntime")
            .field("config", &self.inner.config)
            .finish_non_exhaustive()
    }
}

impl FakeRuntime {
    /// `mcr.microsoft.com/devcontainers/base:debian`: no ENTRYPOINT, CMD `bash`.
    pub const DEBIAN: &'static str = "mcr.microsoft.com/devcontainers/base:debian@sha256:71b6fe7c4e07ec747a94a72b2a17f2f8c286eaa7b1c59862888df764efb8b6c6";
    /// `docker:dind`: ENTRYPOINT `dockerd-entrypoint.sh`.
    pub const DIND: &'static str = "docker:dind";
    /// `alpine:3`: CMD `/bin/sh`.
    pub const ALPINE: &'static str = "alpine:3";

    /// A fake with [`FakeConfig::default`].
    #[must_use]
    pub fn new() -> Self {
        Self::with_config(FakeConfig::default())
    }

    /// A fake with `config`. Knows the images [`FakeRuntime::DEBIAN`], [`FakeRuntime::DIND`]
    /// and [`FakeRuntime::ALPINE`].
    #[must_use]
    pub fn with_config(config: FakeConfig) -> Self {
        let path = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";
        let mut state = State::default();
        let image = |entrypoint: &[&str], cmd: &[&str]| ImageConfig {
            entrypoint: entrypoint.iter().map(|s| (*s).to_owned()).collect(),
            cmd: cmd.iter().map(|s| (*s).to_owned()).collect(),
            env: vec![("PATH".into(), path.into())],
            ..ImageConfig::default()
        };
        state
            .images
            .insert(Self::DEBIAN.into(), image(&[], &["bash"]));
        state
            .images
            .insert(Self::DIND.into(), image(&["dockerd-entrypoint.sh"], &[]));
        state
            .images
            .insert(Self::ALPINE.into(), image(&[], &["/bin/sh"]));
        Self {
            inner: Arc::new(Inner {
                config,
                state: Mutex::new(state),
            }),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Makes `image` pullable with `config`.
    pub fn add_image(&self, image: &ImageRef, config: ImageConfig) {
        self.lock().images.insert(image.to_string(), config);
    }

    /// Arms `fault` for calls of `op`. Faults are checked in the order they were added; the
    /// first one that matches the call decides.
    pub fn inject(&self, op: Op, fault: Fault) {
        self.lock().faults.push((op, fault));
    }

    /// Disarms every fault.
    pub fn clear_faults(&self) {
        self.lock().faults.clear();
    }

    /// Every call so far, in order (including failed ones).
    #[must_use]
    pub fn calls(&self) -> Vec<Call> {
        self.lock().calls.clone()
    }

    /// Makes every call wait once (its future is pending one time) before it answers, the way a
    /// real runtime's calls wait for msb. A test can then drop the caller at each of its awaits.
    pub fn wait_in_every_call(&self, on: bool) {
        self.lock().waits = on;
    }

    /// The wait [`FakeRuntime::wait_in_every_call`] turns on.
    async fn turn(&self) {
        let waits = self.lock().waits;
        if waits {
            WaitOnce(false).await;
        }
    }

    /// Makes every sandbox's `serve_ssh` an SSH server with a pseudo-terminal stand-in (see
    /// [`pty`]) instead of the banner-only fake.
    pub fn serve_terminals(&self) {
        self.lock().terminals = true;
    }

    /// Adds a command handler, tried before earlier handlers and before the built-ins.
    pub fn on_exec(&self, handler: impl ExecHandler) {
        self.lock().handlers.insert(0, Arc::new(handler));
    }

    /// The vsock routes `name` was made with, so a test can speak to the host as its guest would.
    /// Empty when there is no such sandbox.
    #[must_use]
    pub fn routes(&self, name: &SandboxName) -> Vec<crate::VsockRoute> {
        self.lock()
            .sandboxes
            .get(name.as_str())
            .map(|r| r.spec.routes.clone())
            .unwrap_or_default()
    }

    /// The owned disks `name` was made with. Empty when there is no such sandbox.
    #[must_use]
    pub fn owned_disks(&self, name: &SandboxName) -> Vec<crate::OwnedDisk> {
        self.lock()
            .sandboxes
            .get(name.as_str())
            .map(|r| r.spec.owned_disks.clone())
            .unwrap_or_default()
    }

    /// The root disk size `name` was made with: `None` when there is no such sandbox or its spec
    /// named none (the runtime's default).
    #[must_use]
    pub fn root_disk_size(&self, name: &SandboxName) -> Option<DiskSize> {
        self.lock()
            .sandboxes
            .get(name.as_str())
            .and_then(|r| r.spec.root_disk)
    }

    /// The named volumes `name` was made with. Empty when there is no such sandbox.
    #[must_use]
    pub fn volume_mounts(&self, name: &SandboxName) -> Vec<crate::VolumeMount> {
        self.lock()
            .sandboxes
            .get(name.as_str())
            .map(|r| r.spec.volumes.clone())
            .unwrap_or_default()
    }

    /// Simulates the VM dying: a running sandbox becomes `Crashed`. Returns whether it was
    /// running.
    #[must_use = "false means the sandbox wasn't running"]
    pub fn crash(&self, name: &SandboxName) -> bool {
        let mut state = self.lock();
        match state.sandboxes.get_mut(name.as_str()) {
            Some(r) if r.status == WorkspaceStatus::Running => {
                r.status = WorkspaceStatus::Crashed;
                true
            }
            _ => false,
        }
    }

    /// Simulates a VM that outlives its dead owner: the sandbox runs, whatever its handle's
    /// drop did (dropping a handle stops the fake VM, as in the SDK, but a killed process never
    /// gets to drop it). Returns whether the sandbox exists.
    #[must_use = "false means there is no such sandbox"]
    pub fn outlive_owner(&self, name: &SandboxName) -> bool {
        let mut state = self.lock();
        match state.sandboxes.get_mut(name.as_str()) {
            Some(r) => {
                r.status = WorkspaceStatus::Running;
                true
            }
            None => false,
        }
    }

    /// Adds a sandbox Puddle didn't create (another tool's, or one without Puddle's owner label).
    /// [`Runtime::list`] shows it with `puddle_owned: false`; no other operation sees it, so a
    /// test can check through [`FakeRuntime::calls`] that nothing touched it. `name` need not be
    /// a valid [`SandboxName`].
    pub fn add_foreign_sandbox(&self, name: &str, status: WorkspaceStatus) {
        self.lock().foreign.insert(name.to_owned(), status);
    }

    /// Adds a sandbox directory without a record, as a failed create leaves it or as
    /// another tool might; `name` need not be a valid [`SandboxName`].
    pub fn add_stale_dir(&self, name: &str) {
        self.lock().stale_dirs.insert(name.to_owned());
    }

    /// Adds an empty volume under any name, including names that aren't a valid [`VolumeName`]
    /// (another tool's volume).
    pub fn add_foreign_volume(&self, name: &str, size: DiskSize) {
        self.lock().volumes.insert(
            name.to_owned(),
            VolumeRecord {
                size,
                used: None,
                files: Files::new(),
            },
        );
    }

    /// Sets what the volume `name` "takes on the host" from now on; the runtime reports it in
    /// [`VolumeInfo::used`]. A volume that does not exist is left alone.
    pub fn set_volume_used(&self, name: &VolumeName, used: DiskSize) {
        if let Some(volume) = self.lock().volumes.get_mut(name.as_str()) {
            volume.used = Some(used);
        }
    }

    /// Sets how full the root disk of sandbox `name` "is" from now on; the runtime reports it in
    /// [`SandboxInfo::root_disk`]. A sandbox that does not exist is left alone.
    pub fn set_root_disk_use(&self, name: &SandboxName, size: DiskSize, used: DiskSize) {
        if let Some(record) = self.lock().sandboxes.get_mut(name.as_str()) {
            record.root_disk = Some(RootDiskUse { size, used });
        }
    }

    /// Whether a running sandbox grows its root disk live (the default). Off, it answers
    /// [`RootDiskGrowth::NextStart`] and grows at its next boot, as msb does on Windows.
    pub fn set_live_root_grow(&self, live: bool) {
        self.lock().no_live_root_grow = !live;
    }

    fn handle(&self, name: &SandboxName, boot: u64, owned: bool) -> FakeSandbox {
        FakeSandbox {
            runtime: self.clone(),
            name: name.clone(),
            boot,
            owned,
            forced: AtomicBool::new(false),
        }
    }

    /// Makes the next stop of `name` time out gracefully and end the VM by force, as msb does
    /// after 30 seconds: the stop returns `Ok` and [`Sandbox::stopped_by_force`] says `true`.
    pub fn force_next_stop(&self, name: &SandboxName) {
        self.lock().force_stop.insert(name.to_string());
    }
}

impl Runtime for FakeRuntime {
    type Sandbox = FakeSandbox;

    async fn probe(&self) -> Result<Capabilities, ComputeError> {
        self.turn().await;
        self.lock().enter(Op::Probe, None, None)?;
        let c = &self.inner.config;
        Ok(Capabilities {
            runtime_version: c.runtime_version.clone(),
            stale_dir_fixed: true,
            ssh_reports_signal_exit: true,
        })
    }

    async fn pull_image(&self, image: &ImageRef) -> Result<ImageConfig, ComputeError> {
        self.turn().await;
        let mut state = self.lock();
        state.enter(Op::PullImage, Some(image.as_str()), None)?;
        state
            .images
            .get(image.as_str())
            .cloned()
            .ok_or_else(|| ComputeError::ImagePull {
                image: image.to_string(),
                reason: "manifest unknown".into(),
            })
    }

    async fn create(&self, spec: SandboxSpec) -> Result<FakeSandbox, ComputeError> {
        self.turn().await;
        let mut state = self.lock();
        let name = spec.name.clone();
        state.enter(Op::Create, Some(name.as_str()), None)?;
        spec.validate()?;
        for mount in spec
            .file_mounts
            .iter()
            .filter(|m| m.kind == MountKind::Checkout)
        {
            crate::check_checkout_dir(&mount.host).map_err(|why| ComputeError::InvalidSpec {
                reason: format!("the checkout {} {why}", mount.host.display()),
            })?;
        }
        if state.sandboxes.contains_key(name.as_str()) {
            return Err(ComputeError::AlreadyExists {
                sandbox: name.to_string(),
            });
        }
        if state.stale_dirs.contains(name.as_str()) {
            return Err(ComputeError::StaleDir {
                sandbox: name.to_string(),
            });
        }
        if !state.images.contains_key(spec.image.as_str()) {
            return Err(ComputeError::ImagePull {
                image: spec.image.to_string(),
                reason: "manifest unknown".into(),
            });
        }
        for m in &spec.volumes {
            let failure = match (state.volumes.get(m.volume.as_str()), m.ensure_size) {
                (None, None) => Some(ComputeError::VolumeNotFound {
                    volume: m.volume.to_string(),
                }),
                (Some(v), Some(want)) if v.size != want => Some(ComputeError::VolumeSizeMismatch {
                    volume: m.volume.to_string(),
                    existing_mib: v.size.as_mib(),
                    requested_mib: want.as_mib(),
                }),
                _ => None,
            };
            if let Some(e) = failure {
                return Err(e);
            }
        }
        if let Err(e) = state.check_holders(&spec) {
            // msb records the sandbox before the VMM refuses the disk.
            state.sandboxes.insert(
                name.to_string(),
                SandboxRecord {
                    spec,
                    status: WorkspaceStatus::Stopped,
                    boot: 0,
                    files: Files::new(),
                    root_disk: None,
                    pending_root: None,
                    booted_env: GuestEnv::new(),
                },
            );
            return Err(e);
        }
        for m in &spec.volumes {
            if let Some(size) = m.ensure_size {
                state
                    .volumes
                    .entry(m.volume.to_string())
                    .or_insert_with(|| VolumeRecord {
                        size,
                        used: None,
                        files: Files::new(),
                    });
            }
        }
        let boot = state.next_boot();
        let mut record = SandboxRecord {
            spec,
            status: WorkspaceStatus::Created,
            boot: 0,
            files: Files::new(),
            root_disk: None,
            pending_root: None,
            booted_env: GuestEnv::new(),
        };
        record.boot(boot);
        state.sandboxes.insert(name.to_string(), record);
        Ok(self.handle(&name, boot, true))
    }

    async fn start(&self, name: &SandboxName) -> Result<FakeSandbox, ComputeError> {
        self.turn().await;
        let mut state = self.lock();
        state.enter(Op::Start, Some(name.as_str()), None)?;
        let record = state
            .sandboxes
            .get(name.as_str())
            .ok_or_else(|| ComputeError::NotFound {
                sandbox: name.to_string(),
            })?;
        if !record.status.is_down() {
            return Err(ComputeError::InvalidState {
                sandbox: name.to_string(),
                op: "start",
                status: record.status,
            });
        }
        if let Some(m) = record
            .spec
            .volumes
            .iter()
            .find(|m| !state.volumes.contains_key(m.volume.as_str()))
        {
            return Err(ComputeError::VolumeNotFound {
                volume: m.volume.to_string(),
            });
        }
        state.check_holders(&record.spec)?;
        let boot = state.next_boot();
        if let Some(r) = state.sandboxes.get_mut(name.as_str()) {
            r.boot(boot);
        }
        Ok(self.handle(name, boot, true))
    }

    async fn get(&self, name: &SandboxName) -> Result<FakeSandbox, ComputeError> {
        self.turn().await;
        let mut state = self.lock();
        state.enter(Op::Get, Some(name.as_str()), None)?;
        match state.sandboxes.get(name.as_str()) {
            None => Err(ComputeError::NotFound {
                sandbox: name.to_string(),
            }),
            Some(r) if r.status != WorkspaceStatus::Running => Err(ComputeError::InvalidState {
                sandbox: name.to_string(),
                op: "connect to",
                status: r.status,
            }),
            Some(r) => Ok(self.handle(name, r.boot, false)),
        }
    }

    async fn set_memory(&self, name: &SandboxName, memory: MemoryMib) -> Result<(), ComputeError> {
        self.turn().await;
        let mut state = self.lock();
        state.enter(Op::SetMemory, Some(name.as_str()), None)?;
        let record =
            state
                .sandboxes
                .get_mut(name.as_str())
                .ok_or_else(|| ComputeError::NotFound {
                    sandbox: name.to_string(),
                })?;
        record.spec.memory = memory;
        Ok(())
    }

    async fn set_cpus(&self, name: &SandboxName, cpus: CpuCount) -> Result<(), ComputeError> {
        self.turn().await;
        let mut state = self.lock();
        state.enter(Op::SetCpus, Some(name.as_str()), None)?;
        let record =
            state
                .sandboxes
                .get_mut(name.as_str())
                .ok_or_else(|| ComputeError::NotFound {
                    sandbox: name.to_string(),
                })?;
        record.spec.cpus = Some(cpus.get());
        Ok(())
    }

    async fn set_env(&self, name: &SandboxName, env: &GuestEnv) -> Result<(), ComputeError> {
        self.turn().await;
        let mut state = self.lock();
        state.enter(Op::SetEnv, Some(name.as_str()), None)?;
        let record =
            state
                .sandboxes
                .get_mut(name.as_str())
                .ok_or_else(|| ComputeError::NotFound {
                    sandbox: name.to_string(),
                })?;
        record.spec.env = env.clone();
        Ok(())
    }

    async fn grow_root_disk(
        &self,
        name: &SandboxName,
        size: DiskSize,
    ) -> Result<RootDiskGrowth, ComputeError> {
        self.turn().await;
        let mut state = self.lock();
        state.enter(Op::GrowRootDisk, Some(name.as_str()), None)?;
        let live_capable = !state.no_live_root_grow;
        let record =
            state
                .sandboxes
                .get_mut(name.as_str())
                .ok_or_else(|| ComputeError::NotFound {
                    sandbox: name.to_string(),
                })?;
        let current = record
            .root_disk
            .map(|r| r.size)
            .or(record.spec.root_disk)
            .unwrap_or(DiskSize::mib(0));
        if size <= current {
            return Err(ComputeError::InvalidSpec {
                reason: format!(
                    "a root disk only grows: {} MiB is not above {} MiB",
                    size.as_mib(),
                    current.as_mib()
                ),
            });
        }
        record.spec.root_disk = Some(size);
        let running = record.status == WorkspaceStatus::Running;
        if running && !live_capable {
            // The guest keeps what it has until the next boot.
            record.pending_root = Some(size);
            return Ok(RootDiskGrowth::NextStart);
        }
        record.grow_reported(size);
        Ok(if running {
            RootDiskGrowth::Live
        } else {
            RootDiskGrowth::NextStart
        })
    }

    async fn list(&self) -> Result<Vec<SandboxInfo>, ComputeError> {
        self.turn().await;
        let mut state = self.lock();
        state.enter(Op::List, None, None)?;
        let mut out: Vec<SandboxInfo> = state
            .sandboxes
            .iter()
            .map(|(name, r)| SandboxInfo {
                name: name.clone(),
                status: r.status,
                puddle_owned: true,
                root_disk: r.root_disk,
            })
            .chain(state.foreign.iter().map(|(name, status)| SandboxInfo {
                name: name.clone(),
                status: *status,
                puddle_owned: false,
                root_disk: None,
            }))
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    async fn remove(&self, name: &SandboxName) -> Result<(), ComputeError> {
        self.turn().await;
        let mut state = self.lock();
        state.enter(Op::Remove, Some(name.as_str()), None)?;
        match state.sandboxes.get(name.as_str()) {
            None => Err(ComputeError::NotFound {
                sandbox: name.to_string(),
            }),
            Some(r) if !r.status.is_down() => Err(ComputeError::InvalidState {
                sandbox: name.to_string(),
                op: "remove",
                status: r.status,
            }),
            Some(_) => {
                state.sandboxes.remove(name.as_str());
                Ok(())
            }
        }
    }

    async fn stale_dirs(&self) -> Result<Vec<String>, ComputeError> {
        self.turn().await;
        let mut state = self.lock();
        state.enter(Op::StaleDirs, None, None)?;
        Ok(state.stale_dirs.iter().cloned().collect())
    }

    async fn remove_stale_dir(&self, name: &SandboxName) -> Result<(), ComputeError> {
        self.turn().await;
        let mut state = self.lock();
        state.enter(Op::RemoveStaleDir, Some(name.as_str()), None)?;
        if state.stale_dirs.remove(name.as_str()) {
            Ok(())
        } else {
            Err(ComputeError::NotFound {
                sandbox: name.to_string(),
            })
        }
    }

    async fn create_volume(&self, spec: VolumeSpec) -> Result<VolumeInfo, ComputeError> {
        self.turn().await;
        let mut state = self.lock();
        state.enter(Op::CreateVolume, Some(spec.name.as_str()), None)?;
        if spec.size.as_mib() == 0 {
            return Err(ComputeError::InvalidSpec {
                reason: format!("volume {:?} has size 0", spec.name.as_str()),
            });
        }
        if state.volumes.contains_key(spec.name.as_str()) {
            return Err(ComputeError::VolumeExists {
                volume: spec.name.to_string(),
            });
        }
        let record = VolumeRecord {
            size: spec.size,
            used: None,
            files: Files::new(),
        };
        let info = state.volume_info(spec.name.as_str(), &record);
        state.volumes.insert(spec.name.to_string(), record);
        Ok(info)
    }

    async fn volume(&self, name: &VolumeName) -> Result<Option<VolumeInfo>, ComputeError> {
        self.turn().await;
        let mut state = self.lock();
        state.enter(Op::Volume, Some(name.as_str()), None)?;
        Ok(state
            .volumes
            .get(name.as_str())
            .map(|v| state.volume_info(name.as_str(), v)))
    }

    async fn list_volumes(&self) -> Result<Vec<VolumeInfo>, ComputeError> {
        self.turn().await;
        let mut state = self.lock();
        state.enter(Op::ListVolumes, None, None)?;
        Ok(state
            .volumes
            .iter()
            .map(|(name, v)| state.volume_info(name, v))
            .collect())
    }

    async fn remove_volume(&self, name: &VolumeName) -> Result<(), ComputeError> {
        self.turn().await;
        let mut state = self.lock();
        state.enter(Op::RemoveVolume, Some(name.as_str()), None)?;
        if !state.volumes.contains_key(name.as_str()) {
            return Err(ComputeError::VolumeNotFound {
                volume: name.to_string(),
            });
        }
        if let Some(holder) = state.holder_of(name.as_str()) {
            return Err(ComputeError::VolumeInUse {
                volume: name.to_string(),
                holder,
            });
        }
        state.volumes.remove(name.as_str());
        Ok(())
    }
}

/// A handle to a sandbox in a [`FakeRuntime`].
#[derive(Debug)]
pub struct FakeSandbox {
    runtime: FakeRuntime,
    name: SandboxName,
    boot: u64,
    owned: bool,
    forced: AtomicBool,
}

impl FakeSandbox {
    /// Checks that this handle's boot is the running one, for exec and SSH.
    fn check_running(&self, state: &State, op: &'static str) -> Result<(), ComputeError> {
        let r = state
            .sandboxes
            .get(self.name.as_str())
            .ok_or_else(|| ComputeError::NotFound {
                sandbox: self.name.to_string(),
            })?;
        if r.status != WorkspaceStatus::Running {
            return Err(ComputeError::InvalidState {
                sandbox: self.name.to_string(),
                op,
                status: r.status,
            });
        }
        if r.boot != self.boot {
            return Err(ComputeError::StaleHandle {
                sandbox: self.name.to_string(),
            });
        }
        Ok(())
    }

    fn run(&self, state: &mut State, request: &ExecRequest) -> Result<ExecOutput, ComputeError> {
        let handlers = state.handlers.clone();
        let State {
            sandboxes, volumes, ..
        } = state;
        let record =
            sandboxes
                .get_mut(self.name.as_str())
                .ok_or_else(|| ComputeError::NotFound {
                    sandbox: self.name.to_string(),
                })?;
        let mut env = record.booted_env.clone();
        env.extend(&request.env);
        let mut ctx = ExecContext {
            sandbox: &self.name,
            env,
            spec: &record.spec,
            root: &mut record.files,
            volumes,
        };
        if let Some(output) = handlers.iter().find_map(|h| h.handle(&mut ctx, request)) {
            return Ok(output);
        }
        builtin(&mut ctx, request)
    }
}

impl Sandbox for FakeSandbox {
    fn name(&self) -> &SandboxName {
        &self.name
    }

    fn owns_lifecycle(&self) -> bool {
        self.owned
    }

    fn stopped_by_force(&self) -> bool {
        self.forced.load(Ordering::Relaxed)
    }

    async fn status(&self) -> Result<WorkspaceStatus, ComputeError> {
        self.runtime.turn().await;
        let mut state = self.runtime.lock();
        state.enter(Op::Status, Some(self.name.as_str()), None)?;
        state
            .sandboxes
            .get(self.name.as_str())
            .map(|r| r.status)
            .ok_or_else(|| ComputeError::NotFound {
                sandbox: self.name.to_string(),
            })
    }

    async fn stop(&self) -> Result<(), ComputeError> {
        self.runtime.turn().await;
        let mut state = self.runtime.lock();
        state.enter(Op::Stop, Some(self.name.as_str()), None)?;
        let r =
            state
                .sandboxes
                .get_mut(self.name.as_str())
                .ok_or_else(|| ComputeError::NotFound {
                    sandbox: self.name.to_string(),
                })?;
        if r.status.is_down() {
            return Ok(());
        }
        if r.boot != self.boot {
            return Err(ComputeError::StaleHandle {
                sandbox: self.name.to_string(),
            });
        }
        r.status = WorkspaceStatus::Stopped;
        let forced = state.force_stop.remove(self.name.as_str());
        self.forced.store(forced, Ordering::Relaxed);
        Ok(())
    }

    async fn exec(&self, request: ExecRequest) -> Result<ExecOutput, ComputeError> {
        self.runtime.turn().await;
        let mut state = self.runtime.lock();
        let detail = std::iter::once(request.program.as_str())
            .chain(request.args.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" ");
        state.enter(Op::Exec, Some(self.name.as_str()), Some(detail))?;
        self.check_running(&state, "exec")?;
        self.run(&mut state, &request)
    }

    async fn serve_ssh<S: SshStream>(&self, mut stream: S) -> Result<(), ComputeError> {
        self.runtime.turn().await;
        {
            let mut state = self.runtime.lock();
            state.enter(Op::ServeSsh, Some(self.name.as_str()), None)?;
            self.check_running(&state, "serve ssh for")?;
        }
        let io = |e: std::io::Error| ComputeError::Runtime {
            op: "serve ssh",
            message: e.to_string(),
        };
        if self.runtime.lock().terminals {
            return pty::serve(stream)
                .await
                .map_err(|message| ComputeError::Runtime {
                    op: "serve ssh",
                    message,
                });
        }
        stream.write_all(SSH_BANNER.as_bytes()).await.map_err(io)?;
        stream.flush().await.map_err(io)?;
        let mut sink = [0_u8; 4096];
        while stream.read(&mut sink).await.map_err(io)? > 0 {}
        Ok(())
    }
}

/// A future that is pending once, asking to be polled again at once, then ready.
struct WaitOnce(bool);

impl Future for WaitOnce {
    type Output = ();

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<()> {
        if self.0 {
            return std::task::Poll::Ready(());
        }
        self.0 = true;
        cx.waker().wake_by_ref();
        std::task::Poll::Pending
    }
}

impl Drop for FakeSandbox {
    /// Like the SDK (msb 0.7.7): dropping an owning handle shuts the VM down; the record says
    /// `Stopped`.
    fn drop(&mut self) {
        if !self.owned {
            return;
        }
        let mut state = self.runtime.lock();
        if let Some(r) = state.sandboxes.get_mut(self.name.as_str())
            && r.boot == self.boot
            && r.status == WorkspaceStatus::Running
        {
            r.status = WorkspaceStatus::Stopped;
        }
    }
}

pub mod pty;

#[cfg(test)]
mod tests;
