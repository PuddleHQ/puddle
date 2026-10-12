// SPDX-License-Identifier: GPL-3.0-or-later
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
    /// macOS: the probes exist so the report names it, but puddle doesn't run there yet: the
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

/// Whether this process runs in a Windows job object, and what that job allows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JobFacts {
    /// Child processes may leave the job (`JOB_OBJECT_LIMIT_BREAKAWAY_OK` or silent breakaway).
    pub breakaway_allowed: bool,
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
    /// It exists (or may), but its state couldn't be read: access denied, or the status query
    /// failed.
    Unreadable,
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
mod tests {
    use super::*;
    use ServiceState::{Missing, Running, Stopped, Unreadable};

    #[test]
    fn gsa_state_from_its_services() {
        let installed = |running| GsaFacts::Installed { running };
        assert_eq!(GsaFacts::from_services([]), GsaFacts::NotInstalled);
        assert_eq!(
            GsaFacts::from_services([Missing, Missing]),
            GsaFacts::NotInstalled
        );
        assert_eq!(
            GsaFacts::from_services([Missing, Stopped]),
            installed(Some(false))
        );
        assert_eq!(
            GsaFacts::from_services([Stopped, Running, Unreadable]),
            installed(Some(true))
        );
        // An unreadable service might be the running one: "not running" would be a guess.
        assert_eq!(
            GsaFacts::from_services([Stopped, Unreadable, Missing]),
            installed(None)
        );
        assert_eq!(GsaFacts::from_services([Unreadable]), installed(None));
    }

    #[test]
    fn a_failed_code_integrity_query_names_the_status() {
        let CodeIntegrityFacts::Unreadable { detail } =
            CodeIntegrityFacts::query_failed(i32::from_ne_bytes(0xC000_0022_u32.to_ne_bytes()))
        else {
            panic!("expected the unreadable state");
        };
        assert!(detail.contains("NTSTATUS 0xC0000022"), "{detail}");
    }
}
