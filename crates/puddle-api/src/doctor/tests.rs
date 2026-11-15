// SPDX-License-Identifier: AGPL-3.0-or-later
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use puddle_doctor::{Check, CheckId, Report, Status};

use super::*;

fn canned(boot: bool) -> Report {
    Report {
        schema_version: puddle_doctor::SCHEMA_VERSION,
        puddle_version: "9.9.9".into(),
        os: "linux".into(),
        arch: "x86_64".into(),
        ok: true,
        checks: vec![Check {
            id: CheckId::TestBoot,
            status: if boot { Status::Ok } else { Status::Skipped },
            summary: if boot { "booted" } else { "skipped" }.into(),
            finding: None,
            fix: None,
            detail: None,
        }],
        elapsed_ms: 3,
    }
}

#[tokio::test]
async fn without_a_service_the_check_says_it_is_unavailable() {
    let err = NoDoctor.run(true).await.unwrap_err();
    assert!(matches!(err, DoctorError::Unavailable(_)), "{err}");
    assert!(err.to_string().contains("not available"), "{err}");
}

#[tokio::test]
async fn the_host_service_runs_the_checks_and_converts_the_report() {
    let doctor = HostDoctor::new(Arc::new(canned));
    let with_boot = doctor.run(true).await.unwrap();
    assert_eq!(with_boot.puddle_version, "9.9.9");
    assert_eq!(with_boot.checks[0].summary, "booted");
    let without = doctor.run(false).await.unwrap();
    assert_eq!(without.checks[0].status, DoctorStatus::Skipped);
    assert!(format!("{doctor:?}").starts_with("HostDoctor"));
}

#[tokio::test]
async fn a_check_that_panics_is_an_error_not_a_dead_server() {
    let doctor = HostDoctor::new(Arc::new(|boot| {
        if boot {
            // Unwinds without the panic hook: a backtrace written to a slow captured stderr would stall the probe.
            std::panic::resume_unwind(Box::new("probe blew up"));
        }
        canned(boot)
    }));
    let err = doctor.run(true).await.unwrap_err();
    assert!(matches!(err, DoctorError::Crashed(_)), "{err}");
    // The queue is not poisoned: the next run works.
    assert!(doctor.run(false).await.is_ok());
}

#[tokio::test]
async fn runs_queue_one_behind_the_other() {
    let running = Arc::new(AtomicUsize::new(0));
    let most = Arc::new(AtomicUsize::new(0));
    let (r, m) = (running.clone(), most.clone());
    let doctor = Arc::new(HostDoctor::new(Arc::new(move |boot| {
        let now = r.fetch_add(1, Ordering::SeqCst) + 1;
        m.fetch_max(now, Ordering::SeqCst);
        // Simulated slow probe: keeps several in flight so the peak concurrency shows.
        std::thread::sleep(Duration::from_millis(40));
        r.fetch_sub(1, Ordering::SeqCst);
        canned(boot)
    })));
    let runs: Vec<_> = (0..4)
        .map(|_| {
            let doctor = doctor.clone();
            tokio::spawn(async move { doctor.run(false).await })
        })
        .collect();
    for run in runs {
        run.await.unwrap().unwrap();
    }
    assert_eq!(most.load(Ordering::SeqCst), 1, "never two checks at once");
}

#[tokio::test]
async fn the_fake_is_healthy_skips_the_test_boot_when_told_to_and_can_be_replaced() {
    let fake = FakeDoctor::default();
    let full = fake.run(true).await.unwrap();
    assert!(full.ok);
    assert!(full.checks.iter().all(|c| c.status == DoctorStatus::Ok));
    let quick = fake.run(false).await.unwrap();
    let boot = quick.checks.iter().find(|c| c.id == "test_boot").unwrap();
    assert_eq!(boot.status, DoctorStatus::Skipped);

    let mut broken = FakeDoctor::healthy();
    broken.ok = false;
    broken.checks[1].status = DoctorStatus::Fail;
    fake.set(broken.clone());
    assert_eq!(fake.run(true).await.unwrap(), broken);
    assert!(format!("{fake:?}").starts_with("FakeDoctor"));
}
