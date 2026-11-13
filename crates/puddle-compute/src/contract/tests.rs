// SPDX-License-Identifier: AGPL-3.0-or-later
use std::collections::BTreeSet;

use super::*;

#[test]
fn smoke_map_is_complete_and_points_at_real_cases() {
    let checks: BTreeSet<_> = SMOKE_CHECKS.iter().map(|c| c.check).collect();
    assert_eq!(checks.len(), 57, "a check is listed twice");
    for c in &SMOKE_CHECKS {
        match c.coverage {
            Coverage::Case(name) => {
                assert!(CASES.contains(&name), "{}: no case {name}", c.check);
            }
            Coverage::Elsewhere { file, test } => {
                let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("..")
                    .join(file);
                assert!(path.is_file(), "{}: no file {file}", c.check);
                assert!(!test.is_empty(), "{}: no description", c.check);
            }
        }
    }
}

#[test]
fn every_case_has_a_note_and_a_unique_name() {
    let names: BTreeSet<_> = CASES.iter().collect();
    assert_eq!(names.len(), CASES.len());
    assert_eq!(CASE_NOTES.len(), CASES.len());
    for (case, note) in CASES.iter().zip(CASE_NOTES.iter()) {
        assert_eq!(*case, note.0, "CASE_NOTES order differs from CASES");
        assert_ne!(note.1, "");
    }
}

#[test]
fn prefixes_are_short_dns_labels() {
    let p = unique_prefix();
    assert!(p.len() <= 20, "{p}");
    assert!(SandboxName::new(&format!("{p}-26-second")).is_ok(), "{p}");
    assert_eq!(base36(0), "0");
    assert_eq!(base36(35), "z");
    assert_eq!(base36(36), "10");
}

#[test]
fn failure_display_names_the_case() {
    let f = ContractFailure {
        case: "x".into(),
        message: "boom".into(),
    };
    assert_eq!(f.to_string(), "contract case x failed: boom");
    let mut r = ContractReport::default();
    assert!(r.all_passed());
    r.failed.push(f);
    assert!(!r.all_passed());
}
