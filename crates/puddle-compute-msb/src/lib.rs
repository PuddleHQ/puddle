// SPDX-License-Identifier: AGPL-3.0-or-later
//! The compute-plane traits ([`puddle_compute::Runtime`], [`puddle_compute::Sandbox`]) over the
//! msb SDK from Puddle's fork tag (`microsandbox`, git tag `v0.7.7-puddle.N` in the root `Cargo.toml`).
//!
//! Everything above the compute plane codes against the traits; this crate is the only one that
//! talks to the SDK. [`MsbRuntime::open`] takes an [`MsbConfig`] (private msb home, the runtime
//! pair, the guest-share root, SSH keys) and every SDK call runs scoped to that home, so the
//! user's own `~/.microsandbox` is never read or written by these calls.
//!
//! # What the adapter adds on top of the SDK
//!
//! | Contract point | How |
//! |---|---|
//! | Errors are [`puddle_compute::ComputeError`] | [`error::map`]: SDK variants first, messages only where the SDK has no variant |
//! | A handle belongs to one boot (`StaleHandle`) | the handle remembers the VM process id of its boot and compares it on every exec, SSH and stop |
//! | Exit codes exact, signals `-1` | passed through from the SDK |
//! | `VolumeInUse` names the holder | Puddle looks it up: the running sandbox whose config mounts the volume (msb's own refusal doesn't name it) |
//! | `VolumeNotFound` / `VolumeSizeMismatch` | checked against the volume catalog before the SDK create |
//! | No stale directory after a failed create | a create that fails removes the sandbox directory it left (fixed upstream in v0.7.12, `superradcompany/microsandbox#1929`; the contract suite checks it), so [`puddle_compute::Capabilities::stale_dir_fixed`] is `true` |
//! | A start the runtime cannot vouch for is a named refusal | msb keeps the root-disk journal out of the guest's reach since v0.7.12 (upstream advisory GHSA-3f94-vm6x-3jv3) and refuses to start a sandbox whose stored disk state it cannot prove after an upgrade ("migration/recovery required"); [`error::map`] turns that into [`puddle_compute::StartRefusal::RootDiskNeedsRecovery`] |
//! | Mount sources | every file or directory mount's host path must lie under [`MsbConfig::guest_share`]; the one exception, a checkout's `.git` ([`puddle_compute::MountKind::Checkout`]), must be its canonical path, a real git directory outside Puddle's folders, owned by this user on Unix, in a sandbox with no route |
//! | Read-only mounts | every file, directory and checkout mount is passed to msb read-only, so msb's file server refuses writes whatever the guest does to its mount flags |
//! | Memory | `--memory` from the spec at create; [`puddle_compute::Runtime::set_memory`] persists a new size for the next start; `--max-memory` is never set |
//! | CPUs | `--cpus` from the spec at create (when given); [`puddle_compute::Runtime::set_cpus`] persists a new count for the next start; `--max-cpus` is never set apart from that pin |
//! | Environment | `SandboxBuilder::env` at create; [`puddle_compute::Runtime::set_env`] writes the changed variables for the next start with `modify().env()/remove_env().next_start()`, so a changed variable keeps the sandbox and its root disk |
//!
//! # SDK options
//!
//! [`SDK_OPTIONS`] lists every `SandboxBuilder` option Puddle sets and every one it leaves at the
//! SDK default, with the default, for the escape-hardening review. A unit test keeps it in
//! step with [`spec::builder`].
#![forbid(unsafe_code)]

mod checkout;
mod config;
pub mod error;
mod grow_orphans;
mod image;
mod logs;
mod root_disk;
mod runtime;
mod sandbox;
pub mod spec;
mod volume;

pub use config::{DEFAULT_RUNTIME_LOG_LEVEL, MsbConfig, SshConfig};
pub use runtime::MsbRuntime;
pub use sandbox::MsbSandbox;
pub use spec::{SDK_OPTIONS, SdkOption, SdkSetting};

/// The label every sandbox Puddle creates carries (value [`OWNER_LABEL_VALUE`]), so a reconcile
/// can tell Puddle's sandboxes from a user's own.
pub const OWNER_LABEL: &str = "dev.puddle.owner";

/// The value of [`OWNER_LABEL`].
pub const OWNER_LABEL_VALUE: &str = "puddle";
