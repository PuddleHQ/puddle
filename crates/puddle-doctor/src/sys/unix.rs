// SPDX-License-Identifier: AGPL-3.0-or-later
//! What every Unix shares: file permissions, and the Windows-only checks reporting nothing.
//! `linux.rs` and `macos.rs` hold the hypervisor probes.

use std::io;
use std::path::Path;

use crate::facts::{AgentService, CodeIntegrityFacts, GsaFacts, JobFacts};

/// Hypervisor facts for a Unix that is neither Linux nor macOS: no API is known.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn hypervisor() -> crate::facts::HypervisorFacts {
    use crate::facts::{HypervisorApi, HypervisorFacts};
    HypervisorFacts {
        api: HypervisorApi::Unsupported,
        firmware_virtualization: None,
        hypervisor_vendor: super::hypervisor_vendor(),
    }
}

pub(crate) fn code_integrity() -> Option<CodeIntegrityFacts> {
    None
}

pub(crate) fn job() -> Option<JobFacts> {
    None
}

pub(crate) fn global_secure_access() -> Option<GsaFacts> {
    None
}

/// The Windows agent service does not exist here.
pub(crate) fn ssh_agent_service() -> Option<AgentService> {
    None
}

/// Whether this user may read and run `path`.
pub(crate) fn file_access(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let file = std::fs::File::open(path)?;
    if file.metadata()?.permissions().mode() & 0o111 == 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "not executable",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
