// SPDX-License-Identifier: GPL-3.0-or-later
//! The development certificate on this host: `puddle-certs`' manager over the host's trust store
//! (the current user's certificate stores on Windows), served to the API's first-run flow.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use puddle_api::DevCertService;
use puddle_api::wire::DevCertificate;
use puddle_certs::devcert::{DevCertManager, DevCertStatus, DevCertTrust, HostTrust};
use puddle_store::Clock;

/// Where the development certificate stands, read from the host's stores and the data folder.
pub(crate) struct HostDevCert<T = HostTrust> {
    manager: DevCertManager<T>,
    clock: Arc<dyn Clock>,
}

impl<T: DevCertTrust> HostDevCert<T> {
    pub(crate) fn new(data_dir: &Path, trust: T, clock: Arc<dyn Clock>) -> Self {
        Self {
            manager: DevCertManager::new(data_dir, trust),
            clock,
        }
    }
}

impl<T: DevCertTrust> DevCertService for HostDevCert<T> {
    fn status(&self) -> DevCertificate {
        let now = UNIX_EPOCH + Duration::from_millis(self.clock.now_ms());
        wire(self.manager.status(now))
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
pub(crate) fn host_dev_cert(data_dir: &Path, clock: Arc<dyn Clock>) -> Arc<HostDevCert> {
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
        assert!(Empty.export_key(b"cert").is_err());
        let unsupported = HostDevCert::new(dir.path(), puddle_certs::devcert::Unsupported, clock);
        assert_eq!(unsupported.status(), DevCertificate::Unsupported);
    }
}
