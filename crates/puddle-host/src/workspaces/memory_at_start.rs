// SPDX-License-Identifier: AGPL-3.0-or-later
//! The check, before a start, that the host has the memory the workspace is set to.
//!
//! A warning, never a refusal: the figure the OS gives is an estimate that moves with the load,
//! and a VM's memory is charged as it is touched, so a start that looks tight often works. The
//! warning names the way out instead.

use puddle_compute::Runtime;
use puddle_services::workspaces::WorkspaceRecord;
use puddle_types::Problem;

use super::{HostWorkspaces, problem_key};

/// Whether `wanted_mib` of memory fits in what the host reports as `available` bytes. A host that
/// does not say fits everything.
fn fits(wanted_mib: u32, available: Option<u64>) -> bool {
    available.is_none_or(|bytes| u64::from(wanted_mib) * 1024 * 1024 <= bytes)
}

impl<R: Runtime + Clone> HostWorkspaces<R> {
    /// Raises the `start-memory` problem when the host has less memory available than `record` is
    /// set to, and clears it when it has enough.
    pub(super) async fn check_memory_for_start(&self, record: &WorkspaceRecord) {
        let available = tokio::task::spawn_blocking(puddle_fs::available_memory_bytes)
            .await
            .ok()
            .and_then(Result::ok);
        let key = problem_key("start-memory", &record.id);
        let wanted = record.memory.get();
        if fits(wanted, available) {
            self.inner.problems.clear(&key);
            return;
        }
        tracing::warn!(workspace = %record.id, wanted_mib = wanted, ?available, "start passes the host's available memory");
        self.inner.problems.raise(
            Problem::new(
                key,
                format!(
                    "{} may not fit in this computer's memory",
                    self.label(&record.id)
                ),
                format!(
                    "It is set to {} GiB, and this computer has about {} GiB available. Puddle \
                     started it anyway; if it stalls or the computer slows down, stop other \
                     workspaces or lower this one's memory in its settings (it applies at the \
                     next start).",
                    wanted / 1024,
                    available.unwrap_or(0) / (1024 * 1024 * 1024)
                ),
            )
            .about(record.id.clone()),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::fits;

    #[test]
    fn a_start_fits_up_to_exactly_what_is_available_and_an_unknown_host_fits_everything() {
        let gib = 1024_u64 * 1024 * 1024;
        assert!(fits(8 * 1024, Some(8 * gib)));
        assert!(!fits(8 * 1024 + 1, Some(8 * gib)));
        assert!(!fits(256, Some(0)));
        assert!(fits(1024 * 1024, None));
    }
}
