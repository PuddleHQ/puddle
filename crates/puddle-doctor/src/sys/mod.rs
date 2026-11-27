// SPDX-License-Identifier: AGPL-3.0-or-later
//! The platform probes behind [`crate::SystemProbe`]: the same functions on every OS, the OS
//! calls in `windows.rs` (the crate's only `unsafe`), `linux.rs` and `macos.rs`; `unix.rs` holds
//! what the Unixes share.

// The parser is built on Linux for tests only, so real macOS captures run on every CI host.
#[cfg(any(target_os = "macos", all(test, unix)))]
mod acl;
#[cfg(unix)]
mod ancestors;
mod encryption;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(unix)]
pub(crate) use ancestors::data_path;
pub(crate) use encryption::disk_encryption;
#[cfg(target_os = "linux")]
pub(crate) use linux::hypervisor;
#[cfg(target_os = "macos")]
pub(crate) use macos::hypervisor;
#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
pub(crate) use unix::hypervisor;
#[cfg(unix)]
pub(crate) use unix::{code_integrity, file_access, job, problematic_software, ssh_agent_service};
#[cfg(windows)]
pub(crate) use windows::{
    code_integrity, file_access, hypervisor, job, problematic_software, ssh_agent_service,
};

/// msb's rule about the folders above its catalog is read on Linux and macOS; Windows has none.
#[cfg(not(unix))]
pub(crate) fn data_path(_dir: Option<&std::path::Path>) -> crate::facts::DataPathFacts {
    crate::facts::DataPathFacts::NoFolder
}

#[cfg(not(any(unix, windows)))]
compile_error!("puddle-doctor supports unix and windows hosts");

/// The vendor of the hypervisor this OS runs under or beside, from CPUID (`None` when the
/// hypervisor-present bit is clear or the CPU isn't x86-64).
#[must_use]
pub(crate) fn hypervisor_vendor() -> Option<String> {
    #[cfg(target_arch = "x86_64")]
    {
        use core::arch::x86_64::__cpuid;
        // Leaf 1, ECX bit 31: a hypervisor is present.
        if __cpuid(1).ecx & (1 << 31) == 0 {
            return None;
        }
        let r = __cpuid(0x4000_0000);
        let mut bytes = Vec::with_capacity(12);
        for reg in [r.ebx, r.ecx, r.edx] {
            bytes.extend_from_slice(&reg.to_le_bytes());
        }
        Some(vendor_name(&bytes))
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        None
    }
}

/// The printable part of a CPUID vendor signature, or `unknown`.
#[cfg(target_arch = "x86_64")]
fn vendor_name(bytes: &[u8]) -> String {
    let s: String = bytes
        .iter()
        .take_while(|b| **b != 0)
        .filter(|b| b.is_ascii_graphic() || **b == b' ')
        .map(|b| char::from(*b))
        .collect();
    let s = s.trim();
    if s.is_empty() {
        "unknown".to_owned()
    } else {
        s.to_owned()
    }
}

#[cfg(test)]
mod tests;
