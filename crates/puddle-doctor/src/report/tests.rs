// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

fn report(checks: Vec<Check>) -> Report {
    Report {
        schema_version: SCHEMA_VERSION,
        puddle_version: "0.0.0".into(),
        os: "linux".into(),
        arch: "x86_64".into(),
        ok: checks.iter().all(|c| c.status != Status::Fail),
        checks,
        elapsed_ms: 2345,
    }
}

#[test]
fn json_carries_the_schema_version_and_omits_empty_fields() {
    let r = report(vec![Check::new(CheckId::Runtime, Status::Ok, "fine")]);
    let v: serde_json::Value = serde_json::from_str(&r.to_json()).unwrap();
    assert_eq!(v["schema_version"], SCHEMA_VERSION);
    assert_eq!(v["checks"][0]["id"], "runtime");
    assert_eq!(v["checks"][0]["status"], "ok");
    assert!(v["checks"][0].get("fix").is_none());
    assert!(v["checks"][0].get("finding").is_none());
}

#[test]
fn verdict_counts_problems_and_warnings() {
    let fail = Check::new(CheckId::Runtime, Status::Fail, "x");
    let warn = Check::new(CheckId::GlobalSecureAccess, Status::Warn, "y");
    assert!(
        report(vec![])
            .to_text()
            .contains("No problems found. (2.3 s)")
    );
    assert!(
        report(vec![warn.clone()])
            .to_text()
            .contains("No problems found; 1 warning to know about.")
    );
    assert!(
        report(vec![fail.clone(), fail.clone()])
            .to_text()
            .contains("2 problems to fix before Puddle can run sandboxes.")
    );
    assert!(
        report(vec![fail, warn.clone(), warn])
            .to_text()
            .contains("1 problem to fix before Puddle can run sandboxes; 2 warnings.")
    );
}

#[test]
fn blank_detail_is_dropped() {
    let c = Check::new(CheckId::Launch, Status::Fail, "x").detail("  \n");
    assert_eq!(c.detail, None);
}

#[test]
fn prose_wraps_and_commands_do_not() {
    let long = "word ".repeat(30);
    let pieces = wrap(long.trim_end());
    assert!(pieces.len() > 1);
    assert!(pieces.iter().all(|p| p.chars().count() <= WRAP));
    assert_eq!(pieces.join(" "), long.trim_end());
    let cmd = format!("  {}", "x".repeat(120));
    assert_eq!(wrap(&cmd), std::slice::from_ref(&cmd));
    let word = "y".repeat(120);
    assert_eq!(wrap(&word), std::slice::from_ref(&word));
}

#[test]
fn lookups() {
    let r = report(vec![Check::new(CheckId::Runtime, Status::Fail, "x")]);
    assert_eq!(r.failures().count(), 1);
    assert!(r.check(CheckId::Runtime).is_some());
    assert!(r.check(CheckId::TestBoot).is_none());
}
