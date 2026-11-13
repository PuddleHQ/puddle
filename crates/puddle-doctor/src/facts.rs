// SPDX-License-Identifier: AGPL-3.0-or-later
//! What a [`crate::Probe`] reports: plain data, no OS handles, so tests can fake every case and
//! [`crate::diagnose`] can turn it into findings on any platform.

use std::path::PathBuf;
use std::time::Duration;

/// The operating system the checks are worded for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    /// Windows: WHP, AppLocker/WDAC, Global Secure Access.
    Windows,
    /// Linux: KVM (a supported second host).
    Linux,
    /// macOS: the probes exist so the report names it, but Puddle doesn't run there yet: the
    /// hypervisor check says "not supported yet".
    MacOs,
    /// Anything else: only the portable checks.
    Other,
}

impl Os {
    /// The OS this build runs on.
    #[must_use]
    pub fn current() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else if cfg!(target_os = "linux") {
            Self::Linux
        } else if cfg!(target_os = "macos") {
            Self::MacOs
        } else {
            Self::Other
        }
    }

    /// Lower-case name for reports (`windows`, `linux`, `macos`, `other`).
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Windows => "windows",
            Self::Linux => "linux",
            Self::MacOs => "macos",
            Self::Other => "other",
        }
    }
}

/// The hypervisor API msb uses: WHP on Windows, `/dev/kvm` on Linux.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HypervisorApi {
    /// Usable: WHP reports a running hypervisor, or `/dev/kvm` opens read-write.
    Ready,
    /// Not installed: `WinHvPlatform.dll` doesn't load (the optional feature is off), or there is
    /// no `/dev/kvm`.
    NotInstalled {
        /// The loader's or the file system's error.
        detail: String,
    },
    /// WHP is installed but no hypervisor runs in this boot session.
    NotRunning,
    /// `/dev/kvm` exists but this user can't open it.
    AccessDenied {
        /// The error.
        detail: String,
    },
    /// The query itself failed in an unexpected way.
    QueryFailed {
        /// What failed.
        detail: String,
    },
    /// No hypervisor API is known for this OS.
    Unsupported,
}

/// Hardware virtualization and hypervisor facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HypervisorFacts {
    /// The hypervisor API's state.
    pub api: HypervisorApi,
    /// Whether the firmware has CPU virtualization (VT-x / AMD-V) on; `None` when unknown.
    /// Windows can't read it reliably while its own hypervisor runs, so only trust it when
    /// [`HypervisorApi::Ready`] is not the case.
    pub firmware_virtualization: Option<bool>,
    /// The vendor of a hypervisor this OS runs *under* or *beside* (CPUID leaf `0x40000000`),
    /// e.g. `Microsoft Hv`, `VMwareVMware`, `KVMKVMKVM`; `None` when CPUID reports none.
    pub hypervisor_vendor: Option<String>,
}

/// Windows code integrity state (`NtQuerySystemInformation(SystemCodeIntegrityInformation)`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "four independent flags of one Windows bit field"
)]
pub struct CodeIntegrity {
    /// Memory integrity (HVCI) enforces kernel code integrity.
    pub hvci: bool,
    /// App Control for Business (WDAC) enforces a policy for user-mode programs.
    pub user_mode_enforced: bool,
    /// The user-mode policy only audits.
    pub user_mode_audit: bool,
    /// Test signing is on.
    pub test_signing: bool,
}

/// What reading the Windows code integrity state gave.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodeIntegrityFacts {
    /// The state was read.
    Read(CodeIntegrity),
    /// The query failed, so the state is unknown.
    Unreadable {
        /// What failed, with the status code.
        detail: String,
    },
}

impl CodeIntegrityFacts {
    /// The facts for a failed `NtQuerySystemInformation` call that returned `status`.
    #[must_use]
    pub fn query_failed(status: i32) -> Self {
        Self::Unreadable {
            detail: format!(
                "NtQuerySystemInformation(SystemCodeIntegrityInformation) failed: NTSTATUS 0x{:08X}",
                status.cast_unsigned()
            ),
        }
    }
}

/// Why the runtime's file can't be opened for running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessDenied {
    /// The file.
    pub path: PathBuf,
    /// The OS error.
    pub detail: String,
}

/// The bundled runtime's state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeState {
    /// Present, permitted, and exactly the version this build needs.
    Ready {
        /// The version msb embeds.
        version: String,
        /// Accepted only because of the developer override (never in shipped builds).
        overridden: bool,
    },
    /// The file's permissions don't let this user read or run it.
    AccessDenied(AccessDenied),
    /// Missing, unreadable, or another version; the message names the file and both versions.
    Unusable(puddle_runtime::RuntimeError),
}

/// The bundled runtime: where it is and whether it is usable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeFacts {
    /// The runtime folder.
    pub dir: PathBuf,
    /// The msb executable's path.
    pub msb: PathBuf,
    /// The version this build needs.
    pub expected: String,
    /// Its state.
    pub state: RuntimeState,
}

/// How a run of the bundled msb ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcessOutcome {
    /// The process couldn't be created.
    SpawnFailed {
        /// The OS error code (`GetLastError` on Windows, `errno` elsewhere).
        os_error: Option<i32>,
        /// The error's text.
        detail: String,
    },
    /// The process ran and exited.
    Exited {
        /// Its exit code (on Windows an `NTSTATUS` for crashes, as `i32`); `None` when killed by
        /// a signal.
        code: Option<i32>,
        /// The first line of its standard output.
        stdout_first_line: String,
        /// The last lines of its standard error, at most [`crate::TAIL_LINES`].
        stderr_tail: String,
        /// How long it ran.
        elapsed: Duration,
    },
    /// It didn't finish in time and was killed.
    TimedOut {
        /// The time limit.
        after: Duration,
    },
}

/// The test boot: a microVM from a tiny local root file system that runs one probe program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BootFacts {
    /// No test program exists for this CPU architecture.
    UnsupportedArch {
        /// The architecture.
        arch: String,
    },
    /// The root file system or msb home couldn't be prepared on the host.
    SetupFailed {
        /// What failed.
        detail: String,
    },
    /// msb ran.
    Ran {
        /// How it ended (the probe exits with [`crate::PROBE_EXIT_CODE`]).
        outcome: ProcessOutcome,
        /// What became of msb's logs; `None` unless a failed boot was asked to keep them.
        kept_logs: Option<KeptLogs>,
    },
}

/// What became of msb's logs of a failed test boot that was asked to keep them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeptLogs {
    /// The logs went to a new folder.
    Kept {
        /// The folder.
        dir: PathBuf,
        /// The files that could not be copied into it, each with its error; empty when all were.
        failed: Vec<String>,
    },
    /// Nothing was kept.
    NotKept {
        /// Why: msb left no logs, or the folder or the read failed.
        detail: String,
    },
}

/// The free space on the drive that holds Puddle's data folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiskFacts {
    /// The space was read.
    Read {
        /// The folder whose drive was measured.
        dir: PathBuf,
        /// Bytes a normal user can still write there.
        free_bytes: u64,
    },
    /// The folder is known but the OS would not say how much room its drive has.
    Unreadable {
        /// The folder.
        dir: PathBuf,
        /// The OS error.
        detail: String,
    },
    /// Where Puddle keeps its data could not be worked out, so there is nothing to measure.
    NoFolder,
}

/// A folder above Puddle's data that other accounts can add entries to or rename entries in, and
/// that is not sticky: msb refuses to keep its catalog below one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WritableFolder {
    /// The folder, symlinks resolved.
    pub path: PathBuf,
    /// Its permission bits (`0o775`).
    pub mode: u32,
    /// Whether the current user owns it; if not, changing the mode needs `sudo`.
    pub owned: bool,
}

impl WritableFolder {
    /// Whether the group may write here.
    #[must_use]
    pub fn group_writable(&self) -> bool {
        self.mode & 0o020 != 0
    }

    /// Whether everyone may write here.
    #[must_use]
    pub fn world_writable(&self) -> bool {
        self.mode & 0o002 != 0
    }
}

/// Why msb refuses a folder above Puddle's data apart from its permission bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockedBy {
    /// It is not a folder (a file, or another kind of entry).
    NotAFolder,
    /// A user other than this one and root owns it.
    OtherOwner {
        /// The owner's user id.
        uid: u32,
    },
}

/// A folder above Puddle's data that msb refuses because of what it is or who owns it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockedFolder {
    /// The folder, symlinks resolved.
    pub path: PathBuf,
    /// Why msb refuses it.
    pub by: BlockedBy,
}

/// The folders from `/` down to Puddle's data folder, read the way msb reads them before it opens
/// its catalog (Linux only).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DataPathFacts {
    /// The folders were read; `writable` lists those msb would refuse, top first.
    Checked {
        /// The data folder (or its nearest existing parent).
        dir: PathBuf,
        /// The writable folders msb refuses; empty when msb has no objection.
        writable: Vec<WritableFolder>,
        /// The folders msb refuses for what they are or who owns them, top first.
        blocked: Vec<BlockedFolder>,
    },
    /// A folder on the way could not be read.
    Unreadable {
        /// The data folder.
        dir: PathBuf,
        /// The OS error.
        detail: String,
    },
    /// Where Puddle keeps its data could not be worked out, so there is nothing to read.
    NoFolder,
}

/// What the Azure CLI (`az`) on the search path answered for its version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AzCliFacts {
    /// No `az` (`az.cmd` on Windows) is on the search path.
    NotFound,
    /// `az --version` ran and named its version.
    Found {
        /// The version, as `az` prints it (`2.64.0`).
        version: String,
    },
    /// `az` is there but its answer is not one: it failed, hung or printed no version.
    Unknown {
        /// What happened.
        detail: String,
    },
}

/// How the SSH agent Puddle would ask answered a request for its key list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SshAgentFacts {
    /// No agent is named: the settings name none and (not Windows) `SSH_AUTH_SOCK` is not set.
    NoEndpoint,
    /// Nothing can be opened at the endpoint.
    Unreachable {
        /// The pipe or socket asked.
        endpoint: String,
        /// The OS error.
        detail: String,
        /// The OpenSSH Authentication Agent service (Windows); `None` elsewhere.
        service: Option<AgentService>,
        /// The endpoint is the Windows default pipe, which nobody named: an agent that is not
        /// there is a normal state, not a problem.
        by_default: bool,
    },
    /// The agent refused the request, as a locked agent does.
    Locked {
        /// The pipe or socket asked.
        endpoint: String,
    },
    /// The agent did not answer in time.
    TimedOut {
        /// The pipe or socket asked.
        endpoint: String,
        /// The time limit.
        limit: Duration,
    },
    /// The endpoint answered something that is not the SSH agent protocol, or the connection
    /// broke.
    Unusable {
        /// The pipe or socket asked.
        endpoint: String,
        /// What was wrong.
        detail: String,
    },
    /// The agent answered with its keys; only each key's family is kept (`ed25519`, `rsa`), never
    /// a key, a comment or a fingerprint.
    Keys {
        /// The pipe or socket asked.
        endpoint: String,
        /// The family of each key, in the agent's order.
        families: Vec<String>,
    },
}

/// The state of the Windows OpenSSH Authentication Agent service (`ssh-agent`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentService {
    /// The service is not installed.
    Missing,
    /// It is installed and its start type is disabled.
    Disabled,
    /// It is installed, enabled and not running.
    Stopped,
    /// It runs.
    Running,
    /// Its state could not be read.
    Unreadable,
}

impl AgentService {
    /// The state of an installed service from its start type and whether it runs. A running
    /// service counts as running even when its start type is disabled (it was started by hand
    /// before the type changed).
    #[must_use]
    pub fn installed(start_disabled: bool, running: bool) -> Self {
        if running {
            Self::Running
        } else if start_disabled {
            Self::Disabled
        } else {
            Self::Stopped
        }
    }
}

/// The Windows job object this process runs in, when there is one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobFacts {
    /// The job was read.
    Read {
        /// Child processes may leave the job (`JOB_OBJECT_LIMIT_BREAKAWAY_OK` or silent breakaway).
        breakaway_allowed: bool,
    },
    /// Whether there is a job, or what it allows, couldn't be read.
    Unreadable {
        /// The failed call and its OS error.
        detail: String,
    },
}

impl JobFacts {
    /// The facts for a job query `call` that failed with the OS error `os_error`.
    #[must_use]
    pub fn query_failed(call: &str, os_error: u32) -> Self {
        Self::Unreadable {
            detail: format!("{call} failed: os error {os_error}"),
        }
    }

    /// Whether the job is known to forbid child processes leaving it.
    #[must_use]
    pub fn forbids_breakaway(&self) -> bool {
        matches!(
            self,
            Self::Read {
                breakaway_allowed: false
            }
        )
    }
}

/// Microsoft Entra Global Secure Access client state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GsaFacts {
    /// None of its services exist.
    NotInstalled,
    /// At least one of its services exists.
    Installed {
        /// Whether one of them is running; `None` when a service's state couldn't be read and no
        /// other is known to run.
        running: Option<bool>,
    },
    /// The service manager couldn't be asked, so it is unknown whether the client is installed.
    Unknown {
        /// The OS error.
        detail: String,
    },
}

/// What one Global Secure Access service looked like.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceState {
    /// The service doesn't exist.
    Missing,
    /// It exists and isn't running.
    Stopped,
    /// It exists and runs.
    Running,
    /// It exists (or may), but its state couldn't be read: the service couldn't be opened for any
    /// reason but "does not exist" (access denied among them), or the status query failed.
    Unreadable,
}

impl ServiceState {
    /// What a failed `OpenServiceW` with the OS error `os_error` says about the service: only
    /// `ERROR_SERVICE_DOES_NOT_EXIST` (1060) proves it isn't installed. Access denied or any other
    /// failure (a transient one, a busy service manager) proves nothing.
    #[must_use]
    pub fn from_open_error(os_error: u32) -> Self {
        const ERROR_SERVICE_DOES_NOT_EXIST: u32 = 1060;
        if os_error == ERROR_SERVICE_DOES_NOT_EXIST {
            Self::Missing
        } else {
            Self::Unreadable
        }
    }
}

impl GsaFacts {
    /// The client's state from what each of its services looked like: one running service is
    /// enough, and a service that couldn't be read leaves "not running" unproven.
    #[must_use]
    pub fn from_services(services: impl IntoIterator<Item = ServiceState>) -> Self {
        let (mut stopped, mut unreadable) = (false, false);
        for state in services {
            match state {
                ServiceState::Running => {
                    return Self::Installed {
                        running: Some(true),
                    };
                }
                ServiceState::Stopped => stopped = true,
                ServiceState::Unreadable => unreadable = true,
                ServiceState::Missing => {}
            }
        }
        if unreadable {
            Self::Installed { running: None }
        } else if stopped {
            Self::Installed {
                running: Some(false),
            }
        } else {
            Self::NotInstalled
        }
    }
}

#[cfg(test)]
mod tests;

/// Whether what holds data is encrypted at rest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Protection {
    /// An encryption layer covers it.
    Encrypted {
        /// Which: `BitLocker`, `FileVault`, `LUKS2`, `dm-crypt`, `eCryptfs`.
        by: String,
    },
    /// It is on a drive with no encryption layer the probe knows.
    NotEncrypted,
    /// It never reaches a disk (swap in compressed memory, for example).
    NotOnDisk {
        /// What it is instead.
        what: String,
    },
    /// The probe could not tell, or the question does not apply here.
    Unknown {
        /// Why.
        why: String,
    },
}

/// One place the OS pages memory out to: a swap partition or file, or a Windows page file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwapArea {
    /// Its device or file.
    pub name: String,
    /// Whether it is encrypted.
    pub protection: Protection,
}

/// Swap: where the OS pages memory out to, which is where a secret in Puddle's memory can end up
/// on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SwapFacts {
    /// The machine has no swap or page file.
    None,
    /// The swap areas in use.
    Areas(Vec<SwapArea>),
    /// The list of swap areas could not be read.
    Unreadable {
        /// Why.
        why: String,
    },
}

impl SwapFacts {
    /// The swap areas in use; empty when there are none or the list was unreadable.
    #[must_use]
    pub fn areas(&self) -> &[SwapArea] {
        match self {
            Self::Areas(areas) => areas,
            Self::None | Self::Unreadable { .. } => &[],
        }
    }
}

/// Whether the drive that holds Puddle's data folder is encrypted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DataEncryption {
    /// Where Puddle keeps its data could not be worked out.
    NoFolder,
    /// The drive that holds `dir`.
    Volume {
        /// The data folder.
        dir: PathBuf,
        /// Its drive's state.
        protection: Protection,
    },
}

/// Disk encryption: the data folder's drive and the swap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskEncryptionFacts {
    /// Puddle's data folder.
    pub data: DataEncryption,
    /// The swap.
    pub swap: SwapFacts,
}
