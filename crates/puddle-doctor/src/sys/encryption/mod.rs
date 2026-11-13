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

use crate::facts::DiskEncryptionFacts;

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
