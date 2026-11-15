// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn only_a_one_asks_for_dumps() {
    assert!(allows_dumps(Some(OsStr::new("1"))));
    for no in [
        None,
        Some(OsStr::new("")),
        Some(OsStr::new("0")),
        Some(OsStr::new("true")),
        Some(OsStr::new("yes")),
    ] {
        assert!(!allows_dumps(no), "{no:?}");
    }
}

#[test]
fn every_outcome_reads_plainly_and_logs_without_panicking() {
    let shown = [
        (Outcome::Protected, "protected"),
        (Outcome::Allowed, "allowed by the user"),
        (Outcome::Unsupported, "unsupported on this OS"),
        (Outcome::Failed("EPERM".into()), "failed: EPERM"),
    ];
    for (outcome, text) in shown {
        assert_eq!(outcome.to_string(), text);
        outcome.log();
    }
}

#[cfg(target_os = "linux")]
#[test]
fn a_refusing_system_call_leaves_the_process_dumpable_and_says_why() {
    assert_eq!(outcome_of(Ok::<(), String>(())), Outcome::Protected);
    assert_eq!(
        outcome_of(Err("operation not permitted")),
        Outcome::Failed("operation not permitted".into())
    );
}
