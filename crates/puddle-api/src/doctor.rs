// SPDX-License-Identifier: AGPL-3.0-or-later
//! The system check the API serves: the [`DoctorService`] trait the route calls, [`HostDoctor`]
//! that runs the real checks off the async threads one at a time, and [`FakeDoctor`] for tests
//! and the UI fixture.
//!
//! The checks themselves live in `puddle-doctor`; the host hands [`HostDoctor`] a function that
//! runs them against the real machine, so this crate never names the runtime folder.

use std::sync::{Arc, Mutex, PoisonError};

use futures_util::future::BoxFuture;

use crate::wire::{DoctorCheck, DoctorReport, DoctorStatus};

/// Why a report could not be made.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum DoctorError {
    /// The service is not wired in this build or state (503).
    #[error("{0}")]
    Unavailable(String),
    /// The checks themselves crashed (500).
    #[error("the system check stopped unexpectedly: {0}")]
    Crashed(String),
}

/// Runs the system check.
pub trait DoctorService: Send + Sync {
    /// Runs the checks now. `boot` includes the test boot of a tiny virtual machine, the slowest
    /// check (a few seconds, up to 25).
    fn run(&self, boot: bool) -> BoxFuture<'_, Result<DoctorReport, DoctorError>>;
}

/// The service when none is wired in: every call says so.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoDoctor;

impl DoctorService for NoDoctor {
    fn run(&self, _boot: bool) -> BoxFuture<'_, Result<DoctorReport, DoctorError>> {
        Box::pin(async {
            Err(DoctorError::Unavailable(
                "the system check is not available in this build".into(),
            ))
        })
    }
}

/// What runs the checks on the real machine: `boot` says whether to include the test boot.
pub type RunChecks = Arc<dyn Fn(bool) -> puddle_doctor::Report + Send + Sync>;

/// The real system check. Each run happens on a blocking thread (the probes start processes and
/// wait for them), and runs queue up one behind the other, so two open windows never boot two
/// test machines at once.
pub struct HostDoctor {
    run: RunChecks,
    turn: tokio::sync::Mutex<()>,
}

impl std::fmt::Debug for HostDoctor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostDoctor").finish_non_exhaustive()
    }
}

impl HostDoctor {
    /// A service that runs `run` for every request.
    #[must_use]
    pub fn new(run: RunChecks) -> Self {
        Self {
            run,
            turn: tokio::sync::Mutex::new(()),
        }
    }
}

impl DoctorService for HostDoctor {
    fn run(&self, boot: bool) -> BoxFuture<'_, Result<DoctorReport, DoctorError>> {
        Box::pin(async move {
            let _turn = self.turn.lock().await;
            let run = self.run.clone();
            let report = tokio::task::spawn_blocking(move || run(boot))
                .await
                .map_err(|err| DoctorError::Crashed(err.to_string()))?;
            Ok(DoctorReport::from(&report))
        })
    }
}

/// An in-memory [`DoctorService`] for tests and the UI fixture: it serves the report it was
/// given, and [`FakeDoctor::set`] replaces it.
pub struct FakeDoctor {
    report: Mutex<DoctorReport>,
}

impl std::fmt::Debug for FakeDoctor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakeDoctor").finish_non_exhaustive()
    }
}

impl Default for FakeDoctor {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeDoctor {
    /// Serves a healthy machine until [`FakeDoctor::set`] says otherwise.
    #[must_use]
    pub fn new() -> Self {
        Self {
            report: Mutex::new(Self::healthy()),
        }
    }

    /// A report with nothing wrong: what a fresh install on a good computer gets.
    #[must_use]
    pub fn healthy() -> DoctorReport {
        let ok = |id: &str, title: &str, summary: &str| DoctorCheck {
            id: id.to_owned(),
            title: title.to_owned(),
            status: DoctorStatus::Ok,
            summary: summary.to_owned(),
            finding: None,
            fix: None,
            detail: None,
        };
        DoctorReport {
            schema_version: puddle_doctor::SCHEMA_VERSION,
            puddle_version: puddle_types::VERSION.to_owned(),
            os: "linux".to_owned(),
            arch: "x86_64".to_owned(),
            ok: true,
            checks: vec![
                ok(
                    "virtualization",
                    "CPU virtualization",
                    "on (the hypervisor uses it)",
                ),
                ok("hypervisor", "Hypervisor", "KVM is available"),
                ok("runtime", "Bundled runtime", "present, as expected"),
                ok("launch", "Runtime starts", "answered in 0.1 s"),
                ok(
                    "test_boot",
                    "Test boot",
                    "a test VM booted and ran a program in 1.4 s",
                ),
            ],
            elapsed_ms: 1800,
        }
    }

    /// Serves `report` from now on.
    pub fn set(&self, report: DoctorReport) {
        *self.report.lock().unwrap_or_else(PoisonError::into_inner) = report;
    }
}

impl DoctorService for FakeDoctor {
    fn run(&self, boot: bool) -> BoxFuture<'_, Result<DoctorReport, DoctorError>> {
        Box::pin(async move {
            let mut report = self
                .report
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            if !boot {
                // As the real check: a run without the test boot says it didn't look.
                for check in report.checks.iter_mut().filter(|c| c.id == "test_boot") {
                    check.status = DoctorStatus::Skipped;
                    "not checked: --no-boot".clone_into(&mut check.summary);
                }
            }
            Ok(report)
        })
    }
}

#[cfg(test)]
mod tests;
