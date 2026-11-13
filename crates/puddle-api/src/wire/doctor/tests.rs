// SPDX-License-Identifier: AGPL-3.0-or-later
use puddle_doctor::{Check, CheckId, Finding, Report, Status};

use super::*;

fn report(checks: Vec<Check>) -> Report {
    Report {
        schema_version: puddle_doctor::SCHEMA_VERSION,
        puddle_version: "1.2.3".into(),
        os: "linux".into(),
        arch: "x86_64".into(),
        ok: checks.iter().all(|c| c.status != Status::Fail),
        checks,
        elapsed_ms: 1500,
    }
}

fn check(id: CheckId, status: Status) -> Check {
    Check {
        id,
        status,
        summary: "s".into(),
        finding: None,
        fix: None,
        detail: None,
    }
}

#[test]
fn a_report_keeps_its_fields_and_adds_titles_and_nulls() {
    let mut failed = check(CheckId::TestBoot, Status::Fail);
    failed.finding = Some(Finding::BootFailed);
    failed.fix = Some("do this".into());
    let wire = DoctorReport::from(&report(vec![check(CheckId::Runtime, Status::Ok), failed]));
    assert_eq!(wire.schema_version, 1);
    assert_eq!(wire.puddle_version, "1.2.3");
    assert!(!wire.ok);
    assert_eq!(wire.elapsed_ms, 1500);
    assert_eq!(wire.checks.len(), 2);
    let (ok, bad) = (&wire.checks[0], &wire.checks[1]);
    assert_eq!(
        (ok.id.as_str(), ok.title.as_str()),
        ("runtime", "Bundled runtime")
    );
    assert_eq!(ok.status, DoctorStatus::Ok);
    assert_eq!((&ok.finding, &ok.fix, &ok.detail), (&None, &None, &None));
    assert_eq!(bad.id, "test_boot");
    assert_eq!(bad.finding.as_deref(), Some("boot_failed"));
    assert_eq!(bad.fix.as_deref(), Some("do this"));
    assert_eq!(wire.failures().count(), 1);
}

#[test]
fn every_status_has_its_wire_form() {
    for (status, want) in [
        (Status::Ok, DoctorStatus::Ok),
        (Status::Info, DoctorStatus::Info),
        (Status::Warn, DoctorStatus::Warn),
        (Status::Fail, DoctorStatus::Fail),
        (Status::Skipped, DoctorStatus::Skipped),
    ] {
        assert_eq!(DoctorStatus::from(status), want);
        let json = serde_json::to_value(want).unwrap();
        assert_eq!(
            json,
            serde_json::to_value(status).unwrap(),
            "same words as the command line"
        );
    }
}

#[test]
fn a_value_that_is_not_a_string_still_gives_a_code() {
    assert_eq!(code(&7), "7");
}
