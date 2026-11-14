// SPDX-License-Identifier: AGPL-3.0-or-later
//! `puddle doctor`: checks what Puddle needs on this machine and what can get in its
//! way, and says plainly what is wrong and the exact fix. Dev-focused: no admin rights needed to
//! run it, and no IT-process advice.
//!
//! | Check | How | Typical finding |
//! |---|---|---|
//! | CPU virtualization | `IsProcessorFeaturePresent(PF_VIRT_FIRMWARE_ENABLED)`; `/proc/cpuinfo` | off in UEFI/BIOS |
//! | Hypervisor | `WHvGetCapability(HypervisorPresent)` from System32's `WinHvPlatform.dll`; `/dev/kvm`; CPUID vendor | WHP feature off, not running yet, VM without nested virtualization |
//! | Code integrity | `NtQuerySystemInformation(SystemCodeIntegrityInformation)` | HVCI / App Control state (info); unreadable: an info line with the `NTSTATUS` |
//! | Job object | `IsProcessInJob`, `QueryInformationJobObject` | job without breakaway (CI runners; info); unreadable: an info line with the OS error |
//! | Bundled runtime | [`puddle_runtime::BundledRuntime::open`], plus opening `msb` for read + execute | missing, wrong version, permissions |
//! | Runtime starts | `msb --version` with a time limit | `AppLocker`, App Control / Smart App Control, antivirus, EDR, missing DLL, timeout |
//! | Test boot | `msb run` of a 132-byte probe program in a tiny root file system ([`boot`]) | can't boot a VM, timeout; says where a failed boot's logs were kept, or why not |
//! | Disk space | free bytes on the drive that holds Puddle's data folder ([`puddle_fs::free_bytes`]) | under 20 GB warns, under 5 GB fails (both lines are settings) |
//! | Data folder path (Linux) | the mode of every folder from `/` down to Puddle's data folder, by msb's own rule | a group- or world-writable folder that is not sticky: names it and the `chmod g-w` that fixes it |
//! | Disk encryption | `BitLocker` (Windows, read with `PowerShell`; unknown unless the process is elevated), `dm-crypt` below the data folder's drive and swap (Linux, `/sys`), `FileVault` and encrypted swap (macOS) | data drive or swap not encrypted: a secret paged out or kept on disk is readable from the disk; unreadable: an info line |
//! | SSH agent | one key-list request to the agent's pipe or socket ([`puddle_ssh_agent`]), 2 s limit; on Windows also the OpenSSH Authentication Agent service | no agent, locked, no keys, slow to answer (info or warning, never a failure) |
//! | Credential store | a write and delete of a throwaway value, through [`puddle_secrets::probe_os_store`] | no Secret Service provider or a locked keyring (a headless Linux host): saving secrets and sign-ins fails; names the fix and says to restart Puddle |
//! | Global Secure Access | its client's services | installed: explains the VM limitation; service manager unreadable: an info line with the OS error |
//! | Guest kernel (Windows) | none: a fact of the Windows runtime | its guest kernel has `/dev/mem` (info): guest root can map device registers |
//!
//! [`diagnose`] runs the checks against a [`Probe`]: [`SystemProbe`] for the real machine, a fake
//! in tests. The [`Report`] renders as text or as JSON with a [`SCHEMA_VERSION`] (it may be
//! attached to crash reports later).
//!
//! Classification rules for a refused start (who blocked msb):
//!
//! - `ERROR_ACCESS_DISABLED_BY_POLICY` (1260): `AppLocker` or Software Restriction Policies;
//! - `ERROR_SYSTEM_INTEGRITY_*` (4550–4562, 4580–4582), `ERROR_INVALID_IMAGE_HASH` (577),
//!   `STATUS_INVALID_IMAGE_HASH`: App Control for Business (WDAC) or Smart App Control;
//! - `ERROR_VIRUS_INFECTED`/`_DELETED`, `STATUS_VIRUS_INFECTED`: antivirus;
//! - access denied although the file opens for execute: endpoint security (EDR). A deny ACE on
//!   the file is found earlier, by the runtime check, so it is never mistaken for EDR;
//! - access denied while booting inside a job that forbids breakaway: the job, not EDR (hosted CI
//!   runners).
//!
//! Unsafe code exists only in the Windows `sys` module (Win32 calls, each with its reason); elsewhere it is forbidden.
#![cfg_attr(not(windows), forbid(unsafe_code))]

pub mod boot;
mod diagnose;
mod facts;
mod launch;
mod report;
mod sys;
mod system;

pub use diagnose::{
    AZ_CLI_LIMIT, DEFAULT_DISK_FAIL_BELOW, DEFAULT_DISK_WARN_BELOW, Options, PROBE_EXIT_CODE,
    Probe, SSH_AGENT_LIMIT, diagnose,
};
pub use facts::{
    AccessDenied, AgentService, AzCliFacts, BlockedBy, BlockedFolder, BootFacts, CodeIntegrity,
    CodeIntegrityFacts, CredentialStoreFacts, DataEncryption, DataPathFacts, DiskEncryptionFacts,
    DiskFacts, GsaFacts, HypervisorApi, HypervisorFacts, JobFacts, KeptLogs, Os, ProcessOutcome,
    Protection, RuntimeFacts, RuntimeState, ServiceState, SshAgentFacts, SwapArea, SwapFacts,
    VaultFacts, WritableFolder,
};
pub use launch::TAIL_LINES;
pub use puddle_runtime::RuntimeError;
pub use puddle_secrets::{KeySourceTag, LoginSession, StoreHealth};
pub use report::{Check, CheckId, Finding, Report, SCHEMA_VERSION, Status};
pub use system::SystemProbe;
