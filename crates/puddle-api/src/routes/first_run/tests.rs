// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use crate::FakeDevCert;

/// Takes `delay` to answer a trust request, and panics when `panic` is set.
struct Slow {
    delay: Duration,
    panic: bool,
}

impl DevCertService for Slow {
    fn status(&self) -> DevCertificate {
        DevCertificate::NeedsTrust
    }
    fn trust(&self) -> DevCertTrustResult {
        // Simulated slow confirmation, longer than the wait under test.
        std::thread::sleep(self.delay);
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
    let slow = Arc::new(Slow {
        delay: Duration::from_millis(300),
        panic: false,
    });
    let result = trust_within(slow, Duration::from_millis(20)).await;
    assert_eq!(result.dev_certificate, DevCertificate::NeedsTrust);
    assert!(result.waiting);
    assert_eq!(result.detail, None);
}

#[tokio::test]
async fn a_service_that_panics_is_a_failed_attempt_not_a_broken_route() {
    let broken = Arc::new(Slow {
        delay: Duration::ZERO,
        panic: true,
    });
    let result = trust_within(broken, Duration::from_secs(30)).await;
    assert_eq!(result.dev_certificate, DevCertificate::TrustFailed);
    assert!(result.detail.is_some());
}
