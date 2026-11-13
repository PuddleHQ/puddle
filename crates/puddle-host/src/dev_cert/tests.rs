// SPDX-License-Identifier: AGPL-3.0-or-later
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

/// A source with a scripted answer.
struct Scripted(fn() -> Option<GuestDevCert>);

impl GuestCertSource for Scripted {
    fn guest_cert(&self) -> Option<GuestDevCert> {
        (self.0)()
    }
}

fn some_cert() -> GuestDevCert {
    let dir = tempfile::tempdir().unwrap();
    let manager = DevCertManager::new(dir.path(), puddle_certs::devcert::Unsupported);
    GuestDevCert::new(
        &manager
            .ensure(UNIX_EPOCH + Duration::from_hours(500_000))
            .unwrap()
            .material,
    )
}

#[test]
fn the_guest_certificate_is_made_through_the_manager_and_kept_between_calls() {
    let dir = tempfile::tempdir().unwrap();
    let clock = Arc::new(ManualClock::new(1_800_000_000_000));
    let service = HostDevCert::new(dir.path(), Empty, clock);
    let first = service.guest_cert().unwrap();
    // The host user's leaf is the same on the next start; it is not made again.
    assert_eq!(service.guest_cert().unwrap(), first);
    assert!(
        dir.path()
            .join("dev-cert")
            .join(puddle_certs::devcert::LEAF_FILE)
            .is_file()
    );
}

#[test]
fn a_data_folder_that_cannot_hold_the_leaf_gives_no_certificate_not_a_panic() {
    let dir = tempfile::tempdir().unwrap();
    // A file where the folder must go.
    std::fs::write(dir.path().join("dev-cert"), b"in the way").unwrap();
    let clock = Arc::new(ManualClock::new(1_800_000_000_000));
    let service = HostDevCert::new(dir.path(), Empty, clock);
    assert_eq!(service.guest_cert(), None);
}

/// A host whose store lets the user answer: it starts untrusted and a yes (or a no, or a
/// refusal by policy) is what `answer` says.
struct Answering {
    trusted: std::sync::Mutex<bool>,
    answer: Result<TrustOutcome, TrustError>,
    asked: std::sync::atomic::AtomicU32,
}

impl Answering {
    fn new(answer: Result<TrustOutcome, TrustError>) -> Self {
        Self {
            trusted: std::sync::Mutex::new(false),
            answer,
            asked: std::sync::atomic::AtomicU32::new(0),
        }
    }
}

impl DevCertTrust for &Answering {
    fn existing(&self) -> Result<Vec<ExistingCert>, TrustError> {
        Ok(Vec::new())
    }
    fn export_key(&self, _: &[u8]) -> Result<PrivateKey, TrustError> {
        Err(TrustError::Failed("none".into()))
    }
    fn is_trusted(&self, _: &[u8]) -> Result<bool, TrustError> {
        Ok(*self.trusted.lock().unwrap())
    }
    fn trust(&self, _: &[u8]) -> Result<TrustOutcome, TrustError> {
        self.asked.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if self.answer == Ok(TrustOutcome::Trusted) {
            *self.trusted.lock().unwrap() = true;
        }
        self.answer.clone()
    }
}

fn trusting(answering: &Answering) -> (tempfile::TempDir, HostDevCert<&Answering>) {
    let dir = tempfile::tempdir().unwrap();
    let clock = Arc::new(ManualClock::new(1_800_000_000_000));
    let service = HostDevCert::new(dir.path(), answering, clock);
    (dir, service)
}

#[test]
fn pressing_trust_asks_once_and_a_second_press_after_a_yes_asks_nothing() {
    let store = Answering::new(Ok(TrustOutcome::Trusted));
    let (_dir, service) = trusting(&store);
    assert_eq!(service.status(), DevCertificate::NeedsTrust);
    let first = service.trust();
    assert_eq!(first.dev_certificate, DevCertificate::Trusted);
    assert_eq!(first.detail, None);
    assert_eq!(service.status(), DevCertificate::Trusted);
    assert_eq!(service.trust().dev_certificate, DevCertificate::Trusted);
    assert_eq!(store.asked.load(std::sync::atomic::Ordering::SeqCst), 1);
}

/// A store whose confirmation stays open until `answer` is sent.
struct Open {
    entered: std::sync::mpsc::SyncSender<()>,
    answer: std::sync::Mutex<std::sync::mpsc::Receiver<TrustOutcome>>,
    asked: std::sync::atomic::AtomicU32,
}

impl DevCertTrust for &Open {
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
        self.asked.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.entered.send(()).unwrap();
        Ok(self.answer.lock().unwrap().recv().unwrap())
    }
}

#[test]
fn a_press_while_the_confirmation_is_open_asks_nothing_and_says_it_is_waiting() {
    let (entered_tx, entered) = std::sync::mpsc::sync_channel(1);
    let (answer_tx, answer_rx) = std::sync::mpsc::channel();
    let open = Open {
        entered: entered_tx,
        answer: std::sync::Mutex::new(answer_rx),
        asked: std::sync::atomic::AtomicU32::new(0),
    };
    let dir = tempfile::tempdir().unwrap();
    let clock = Arc::new(ManualClock::new(1_800_000_000_000));
    // The double's other answer, so no line of it goes unread.
    let by_ref = &open;
    assert!(by_ref.export_key(b"cert").is_err());
    let service = HostDevCert::new(dir.path(), &open, clock);
    std::thread::scope(|scope| {
        let first = scope.spawn(|| service.trust());
        entered.recv().unwrap();
        let second = service.trust();
        assert_eq!(second.dev_certificate, DevCertificate::NeedsTrust);
        assert!(second.waiting);
        answer_tx.send(TrustOutcome::Declined).unwrap();
        let first = first.join().unwrap();
        assert_eq!(first.dev_certificate, DevCertificate::TrustDeclined);
        assert!(!first.waiting);
    });
    assert_eq!(open.asked.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[test]
fn a_confirmation_that_panicked_does_not_stop_the_next_press() {
    let store = Answering::new(Ok(TrustOutcome::Trusted));
    let (_dir, service) = trusting(&store);
    // The double's other answer, so no line of it goes unread.
    let by_ref = &store;
    assert!(by_ref.export_key(b"cert").is_err());
    // A press that panicked while it held the one-at-a-time lock leaves the lock poisoned.
    std::thread::scope(|scope| {
        let panicked = scope.spawn(|| {
            let _held = service.ensuring.lock().unwrap();
            panic!("the store blew up");
        });
        assert!(panicked.join().is_err());
    });
    assert_eq!(service.trust().dev_certificate, DevCertificate::Trusted);
}

#[test]
fn a_no_is_remembered_and_asked_again_at_the_next_press() {
    let store = Answering::new(Ok(TrustOutcome::Declined));
    let (_dir, service) = trusting(&store);
    assert_eq!(
        service.trust().dev_certificate,
        DevCertificate::TrustDeclined
    );
    assert_eq!(service.status(), DevCertificate::TrustDeclined);
    assert_eq!(
        service.trust().dev_certificate,
        DevCertificate::TrustDeclined
    );
    assert_eq!(store.asked.load(std::sync::atomic::Ordering::SeqCst), 2);
}

#[test]
fn a_refusal_by_policy_is_a_failure_with_the_systems_words() {
    let store = Answering::new(Err(TrustError::Failed("os error 5".into())));
    let (_dir, service) = trusting(&store);
    let result = service.trust();
    assert_eq!(result.dev_certificate, DevCertificate::TrustFailed);
    assert!(result.detail.unwrap().contains("os error 5"));
    assert_eq!(service.status(), DevCertificate::TrustFailed);
}

#[test]
fn a_data_folder_that_cannot_hold_the_leaf_is_a_failed_attempt() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("dev-cert"), b"in the way").unwrap();
    let clock = Arc::new(ManualClock::new(1_800_000_000_000));
    let service = HostDevCert::new(dir.path(), Empty, clock);
    let result = service.trust();
    assert_eq!(result.dev_certificate, DevCertificate::TrustFailed);
    assert!(result.detail.is_some());
}

#[test]
fn the_hosts_own_service_reads_its_state_from_the_data_folder() {
    let dir = tempfile::tempdir().unwrap();
    let clock = Arc::new(ManualClock::new(1_800_000_000_000));
    // Whatever this machine's stores hold, asking never panics and never makes a file.
    let _ = host_dev_cert(dir.path(), clock).status();
    assert!(!dir.path().join("dev-cert").exists());
}

#[tokio::test]
async fn a_start_gets_the_certificate_a_source_gives() {
    let source: Arc<dyn GuestCertSource> = Arc::new(Scripted(|| Some(some_cert())));
    assert!(
        certificate_within(source, Duration::from_secs(30))
            .await
            .is_some()
    );
}

#[tokio::test]
async fn a_start_goes_on_without_a_certificate_when_the_source_has_none_panics_or_is_slow() {
    let none: Arc<dyn GuestCertSource> = Arc::new(Scripted(|| None));
    assert!(
        certificate_within(none, Duration::from_secs(30))
            .await
            .is_none()
    );
    let panics: Arc<dyn GuestCertSource> = Arc::new(Scripted(|| panic!("the store blew up")));
    assert!(
        certificate_within(panics, Duration::from_secs(30))
            .await
            .is_none()
    );
    let slow: Arc<dyn GuestCertSource> = Arc::new(Scripted(|| {
        // Simulated slow source, longer than the timeout under test.
        std::thread::sleep(Duration::from_millis(400));
        Some(some_cert())
    }));
    assert!(
        certificate_within(slow, Duration::from_millis(30))
            .await
            .is_none()
    );
}
