// SPDX-License-Identifier: AGPL-3.0-or-later
//! How much memory the host can still give to new work.

use std::io;

/// Bytes of memory the host can still hand out without pushing something else out. On Linux this
/// is `MemAvailable` from `/proc/meminfo`; on Windows it is the commit the system can still
/// promise (available page file in `GlobalMemoryStatusEx`), the limit a VM's memory is charged
/// against. It is an estimate that moves with the load, so it suits a warning, not a guarantee.
///
/// # Errors
///
/// The OS does not say, or this OS has no reading here (macOS).
pub fn available_memory_bytes() -> io::Result<u64> {
    #[cfg(target_os = "linux")]
    {
        let text = std::fs::read_to_string("/proc/meminfo")?;
        mem_available_bytes(&text).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "no MemAvailable in /proc/meminfo",
            )
        })
    }
    #[cfg(windows)]
    {
        crate::win::available_commit()
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "no available-memory reading on this OS",
        ))
    }
}

/// The `MemAvailable` line of a `/proc/meminfo` text, in bytes.
#[cfg(any(target_os = "linux", test))]
fn mem_available_bytes(meminfo: &str) -> Option<u64> {
    let rest = meminfo
        .lines()
        .find_map(|line| line.strip_prefix("MemAvailable:"))?;
    let kib: u64 = rest.trim().strip_suffix("kB")?.trim().parse().ok()?;
    kib.checked_mul(1024)
}

#[cfg(test)]
mod tests;
