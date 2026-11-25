// SPDX-License-Identifier: AGPL-3.0-or-later
//! Whether the data folder's drive and the swap are encrypted. Each OS reads what it can without
//! administrator rights (on Windows that is nothing for `BitLocker`: the answer is unknown unless the
//! process is elevated): `linux.rs` walks `/sys`, `windows.rs` asks `PowerShell` for the shell's
//! `BitLocker` property, `macos.rs` runs `fdesetup` and `sysctl`.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;
#[cfg(any(windows, test))]
mod windows_parse;

use std::path::Path;
#[cfg(any(windows, target_os = "macos"))]
use std::time::Duration;

use crate::facts::DiskEncryptionFacts;

/// Why a probe could not tell. It displays as the finding's detail line.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum ProbeError {
    /// A file the probe reads could not be read.
    #[cfg(target_os = "linux")]
    #[error("{path}: {cause}")]
    Unreadable {
        /// The file.
        path: &'static str,
        /// The operating system's words.
        cause: String,
    },
    /// The program ran and failed.
    #[cfg(any(windows, target_os = "macos"))]
    #[error("{program} exited with {code:?}{}", stderr.as_ref().map_or_else(String::new, |t| format!(": {t}")))]
    Exited {
        /// The program's name.
        program: String,
        /// Its exit code, if it had one.
        code: Option<i32>,
        /// The first line of what it printed to standard error, when that is asked.
        stderr: Option<String>,
    },
    /// The program did not start.
    #[cfg(any(windows, target_os = "macos"))]
    #[error("{program} did not start: {detail}")]
    NotStarted {
        /// The program's name.
        program: String,
        /// Why not.
        detail: String,
    },
    /// The program did not answer in time.
    #[cfg(any(windows, target_os = "macos"))]
    #[error("{program} did not answer within {after:?}")]
    TimedOut {
        /// The program's name.
        program: String,
        /// How long it was given.
        after: Duration,
    },
}

/// The encryption facts of this machine for the data folder `dir`.
pub(crate) fn disk_encryption(dir: Option<&Path>) -> DiskEncryptionFacts {
    #[cfg(target_os = "linux")]
    {
        linux::disk_encryption(dir)
    }
    #[cfg(target_os = "macos")]
    {
        macos::disk_encryption(dir)
    }
    #[cfg(windows)]
    {
        windows::disk_encryption(dir)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        use crate::facts::{DataEncryption, Protection, SwapFacts};
        let why = "this OS has no encryption probe".to_owned();
        DiskEncryptionFacts {
            data: dir.map_or(DataEncryption::NoFolder, |dir| DataEncryption::Volume {
                dir: dir.to_path_buf(),
                protection: Protection::Unknown { why: why.clone() },
            }),
            swap: SwapFacts::Unreadable { why },
        }
    }
}
