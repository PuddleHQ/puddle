// SPDX-License-Identifier: AGPL-3.0-or-later
//! Keeps the host process out of dump files.
//!
//! The host holds the plaintext of every credential it injects (tokens, pasted secrets, the
//! per-workspace CA keys). A core file or a crash report of that process would carry them to
//! disk, and from there into a bug report or an upload. [`harden_from_env`] closes that route
//! once at start:
//!
//! | OS | What it does | What it does not stop |
//! |---|---|---|
//! | Linux | `prctl(PR_SET_DUMPABLE, 0)`: no core file, and no `ptrace` or `/proc/<pid>/mem` from a process that is not root | root, a debugger started with the privilege |
//! | Windows | `WerAddExcludedApplication` for Puddle's executables: Windows Error Reporting writes no dump | an administrator's dump, Task Manager, procdump, the webview's own crash reports |
//! | other | nothing | everything |
//!
//! Nothing protects against a process of the same user that reads the credential stores
//! directly; this is about files that are written without anyone asking for them.
//!
//! `PUDDLE_ALLOW_DUMPS=1` switches it off for diagnosis: the process stays dumpable and, on
//! Windows, the exclusion an earlier start made is removed again.
#![cfg_attr(not(windows), forbid(unsafe_code))]

use std::ffi::OsStr;
use std::fmt;

#[cfg(windows)]
mod windows;

/// The environment variable that keeps native crash dumps for diagnosis (`1` = keep them).
pub const ALLOW_VAR: &str = "PUDDLE_ALLOW_DUMPS";

/// What [`harden_process`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The process is out of dump files.
    Protected,
    /// The user asked for dumps ([`ALLOW_VAR`]); nothing is protected.
    Allowed,
    /// This operating system has no such switch in Puddle yet.
    Unsupported,
    /// The system refused; the process stays dumpable.
    Failed(String),
}

impl Outcome {
    /// Logs the outcome once: `debug` for the normal protected state (a quiet start), `warn` when
    /// the user opted out, the system refused or this OS is not covered.
    pub fn log(&self) {
        match self {
            Self::Protected => tracing::debug!("process is excluded from core and crash dumps"),
            Self::Allowed => {
                tracing::warn!(
                    "{ALLOW_VAR}=1: core and crash dumps of this process may hold credentials"
                );
            }
            Self::Unsupported => tracing::warn!("dump protection is not available on this OS"),
            Self::Failed(why) => tracing::warn!(%why, "could not exclude the process from dumps"),
        }
    }
}

impl fmt::Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Protected => f.write_str("protected"),
            Self::Allowed => f.write_str("allowed by the user"),
            Self::Unsupported => f.write_str("unsupported on this OS"),
            Self::Failed(why) => write!(f, "failed: {why}"),
        }
    }
}

/// Whether `value` (the content of [`ALLOW_VAR`]) asks for dumps: exactly `1`.
#[must_use]
pub fn allows_dumps(value: Option<&OsStr>) -> bool {
    value == Some(OsStr::new("1"))
}

/// [`harden_process`], with the opt-out read from [`ALLOW_VAR`]. Call it once at start, before
/// the first credential is read, and log the result.
#[must_use]
pub fn harden_from_env() -> Outcome {
    harden_process(allows_dumps(std::env::var_os(ALLOW_VAR).as_deref()))
}

/// Excludes this process from dump files, or, when `allow` is set, leaves it dumpable (and on
/// Windows removes the exclusion an earlier run registered).
#[must_use]
pub fn harden_process(allow: bool) -> Outcome {
    platform(allow)
}

#[cfg(target_os = "linux")]
fn platform(allow: bool) -> Outcome {
    if allow {
        return Outcome::Allowed;
    }
    outcome_of(rustix::process::set_dumpable_behavior(
        rustix::process::DumpableBehavior::NotDumpable,
    ))
}

/// The outcome of the system call that made the process non-dumpable.
#[cfg(target_os = "linux")]
fn outcome_of(result: Result<(), impl fmt::Display>) -> Outcome {
    match result {
        Ok(()) => Outcome::Protected,
        Err(e) => Outcome::Failed(e.to_string()),
    }
}

#[cfg(windows)]
fn platform(allow: bool) -> Outcome {
    windows::platform(allow)
}

#[cfg(not(any(target_os = "linux", windows)))]
fn platform(allow: bool) -> Outcome {
    if allow {
        Outcome::Allowed
    } else {
        Outcome::Unsupported
    }
}

#[cfg(test)]
mod tests;
