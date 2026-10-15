// SPDX-License-Identifier: GPL-3.0-or-later
//! The development certificate on this host: `puddle-certs`' manager over the host's trust store
//! (the current user's certificate stores on Windows), served to the API's first-run flow.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use puddle_api::DevCertService;
use puddle_api::wire::DevCertificate;
use puddle_certs::devcert::{
    DevCertManager, DevCertReport, DevCertStatus, DevCertTrust, GuestDevCert, HostTrust,
};
use puddle_store::Clock;

/// Where a workspace's guest gets the development certificate from, at every boot.
pub trait GuestCertSource: Send + Sync {
    /// The certificate and key for the next boot, or `None` when there is none to give (the
    /// reason is logged). May block: the system can ask the user once (the first time puddle's
    /// own certificate is trusted), so callers run it off the async threads.
    fn guest_cert(&self) -> Option<GuestDevCert>;
}

/// The development certificate on this host: its state for the API and the certificate for the
/// guests. Tests give a double in [`HostOptions`](crate::HostOptions), so they never touch the
/// real certificate stores.
pub trait DevCertSource: GuestCertSource + DevCertService {}

impl<T: GuestCertSource + DevCertService> DevCertSource for T {}

/// Where the development certificate stands, read from the host's stores and the data folder,
/// and what the guests are given.
pub(crate) struct HostDevCert<T = HostTrust> {
    manager: DevCertManager<T>,
    clock: Arc<dyn Clock>,
    /// One `ensure` at a time: two starts must not raise two confirmations.
    ensuring: std::sync::Mutex<()>,
}

impl<T: DevCertTrust> HostDevCert<T> {
    pub(crate) fn new(data_dir: &Path, trust: T, clock: Arc<dyn Clock>) -> Self {
        Self {
            manager: DevCertManager::new(data_dir, trust),
            clock,
            ensuring: std::sync::Mutex::new(()),
        }
    }

    fn now(&self) -> std::time::SystemTime {
        UNIX_EPOCH + Duration::from_millis(self.clock.now_ms())
    }
}

impl<T: DevCertTrust> GuestCertSource for HostDevCert<T> {
    fn guest_cert(&self) -> Option<GuestDevCert> {
        let _one_at_a_time = self
            .ensuring
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match self.manager.ensure(self.now()) {
            Ok(report) => {
                log_report(&report);
                Some(GuestDevCert::new(&report.material))
            }
            Err(error) => {
                tracing::warn!(%error, "the workspace gets no development certificate this time");
                None
            }
        }
    }
}

/// One line for the log: where it stands, whose certificate and what was passed over (names and
/// reasons only, never key material).
fn log_report(report: &DevCertReport) {
    let skipped: Vec<String> = report
        .skipped
        .iter()
        .map(|s| format!("{}: {}", s.fingerprint, s.reason))
        .collect();
    tracing::info!(
        status = ?report.status,
        origin = ?report.material.origin,
        detail = report.detail.as_deref().unwrap_or(""),
        passed_over = ?skipped,
        "development certificate for the workspace"
    );
}

impl<T: DevCertTrust> DevCertService for HostDevCert<T> {
    fn status(&self) -> DevCertificate {
        wire(self.manager.status(self.now()))
    }
}

/// The API's name for a state.
fn wire(status: DevCertStatus) -> DevCertificate {
    match status {
        DevCertStatus::ReusingExisting => DevCertificate::ReusingExisting,
        DevCertStatus::Trusted => DevCertificate::Trusted,
        DevCertStatus::NeedsTrust => DevCertificate::NeedsTrust,
        DevCertStatus::TrustDeclined => DevCertificate::TrustDeclined,
        DevCertStatus::TrustFailed => DevCertificate::TrustFailed,
        DevCertStatus::Unsupported => DevCertificate::Unsupported,
        // A state added later must be named here before it can reach the API.
        _ => DevCertificate::NotChecked,
    }
}

/// The real service for this host.
pub(crate) fn host_dev_cert(data_dir: &Path, clock: Arc<dyn Clock>) -> Arc<dyn DevCertSource> {
    Arc::new(HostDevCert::new(data_dir, HostTrust::default(), clock))
}

#[cfg(test)]
mod tests {
    use puddle_certs::devcert::{ExistingCert, PrivateKey, TrustError, TrustOutcome};
    use puddle_store::ManualClock;

    use super::*;

    #[test]
    fn every_state_has_a_wire_name() {
        let all = [
            (
                DevCertStatus::ReusingExisting,
                DevCertificate::ReusingExisting,
            ),
            (DevCertStatus::Trusted, DevCertificate::Trusted),
            (DevCertStatus::NeedsTrust, DevCertificate::NeedsTrust),
            (DevCertStatus::TrustDeclined, DevCertificate::TrustDeclined),
            (DevCertStatus::TrustFailed, DevCertificate::TrustFailed),
            (DevCertStatus::Unsupported, DevCertificate::Unsupported),
        ];
        for (status, wire_name) in all {
            assert_eq!(wire(status), wire_name);
        }
    }

    /// A host with an empty personal store that trusts nothing.
    struct Empty;

    impl DevCertTrust for Empty {
        fn existing(&self) -> Result<Vec<ExistingCert>, TrustError> {
            Ok(Vec::new())
        }
        fn export_key(&self, _: &[u8]) -> Result<PrivateKey, TrustError> {
            Err(TrustError::Failed("none".into()))
        }
        fn is_trusted(&self, _: &[u8]) -> Result<bool, TrustError> {
            Ok(false)
        }
        fn trust(&self, _: &[u8]) -> Result<TrustOutcome, TrustError> {
            Ok(TrustOutcome::Declined)
        }
    }

    #[test]
    fn the_service_reads_the_manager_with_the_hosts_clock() {
        let dir = tempfile::tempdir().unwrap();
        let clock = Arc::new(ManualClock::new(1_800_000_000_000));
        let service = HostDevCert::new(dir.path(), Empty, clock.clone());
        assert_eq!(service.status(), DevCertificate::NeedsTrust);
        // The double's other answers, so no line of it goes unread.
        assert!(Empty.export_key(b"cert").is_err());
        assert_eq!(Empty.is_trusted(b"cert"), Ok(false));
        assert_eq!(Empty.trust(b"cert"), Ok(TrustOutcome::Declined));
        let unsupported = HostDevCert::new(dir.path(), puddle_certs::devcert::Unsupported, clock);
        assert_eq!(unsupported.status(), DevCertificate::Unsupported);
    }
}
