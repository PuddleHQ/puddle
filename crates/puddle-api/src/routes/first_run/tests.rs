// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use puddle_services::dev_cert::FakeDevCert;

/// Answers a trust request only when `gate` is released (its sender sends or is dropped), and
/// panics then when `panic` is set. A test holds the sender to keep the answer pending for as
/// long as it likes, however long the machine stalls.
struct Slow {
    gate: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
    panic: bool,
}

impl Slow {
    /// A service that answers once the returned sender is dropped.
    fn gated(panic: bool) -> (Arc<Self>, std::sync::mpsc::Sender<()>) {
        let (release, gate) = std::sync::mpsc::channel();
        let service = Arc::new(Self {
            gate: std::sync::Mutex::new(gate),
            panic,
        });
        (service, release)
    }
}

impl DevCertService for Slow {
    fn status(&self) -> DevCertificate {
        DevCertificate::NeedsTrust
    }
    fn trust(&self) -> DevCertTrustResult {
        // The simulated confirmation: unanswered until the test releases it.
        let _ = self.gate.lock().unwrap().recv();
        assert!(!self.panic, "the store blew up");
        DevCertTrustResult {
            dev_certificate: DevCertificate::Trusted,
            detail: None,
            waiting: false,
        }
    }
}

#[tokio::test]
async fn an_answer_in_time_is_passed_on() {
    let fake = Arc::new(FakeDevCert::new(DevCertificate::NeedsTrust));
    let result = trust_within(fake, Duration::from_secs(30)).await;
    assert_eq!(result.dev_certificate, DevCertificate::Trusted);
    assert_eq!(result.detail, None);
}

#[tokio::test]
async fn an_unanswered_confirmation_leaves_the_state_as_it_is_and_says_so() {
    let (slow, release) = Slow::gated(false);
    let result = trust_within(slow, Duration::from_millis(20)).await;
    assert_eq!(result.dev_certificate, DevCertificate::NeedsTrust);
    assert!(result.waiting);
    assert_eq!(result.detail, None);
    // Let the blocking thread finish so the runtime can shut down.
    drop(release);
}

#[tokio::test]
async fn a_service_that_panics_is_a_failed_attempt_not_a_broken_route() {
    let (broken, release) = Slow::gated(true);
    drop(release);
    let result = trust_within(broken, Duration::from_secs(30)).await;
    assert_eq!(result.dev_certificate, DevCertificate::TrustFailed);
    assert!(result.detail.is_some());
}
