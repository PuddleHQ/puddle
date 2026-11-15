// SPDX-License-Identifier: AGPL-3.0-or-later
use std::fmt::Write as _;

use super::*;

#[test]
fn removed_locks_and_errors_are_collected() {
    let r = parse("L\tapi/.git/index.lock\nL\tapi/.git/refs/heads/a b.lock\nE\tweb/.git/x.lock\tcannot remove\nD\n")
            .unwrap();
    assert!(!r.skipped_busy);
    assert_eq!(
        r.removed,
        ["api/.git/index.lock", "api/.git/refs/heads/a b.lock"]
    );
    assert_eq!(r.errors, ["web/.git/x.lock: cannot remove"]);
    assert_eq!(r.removed_count(), 2);
}

#[test]
fn repairs_and_left_alone_damage_are_collected() {
    let r = parse(
            "R\tapi/.git/HEAD\tHEAD was zero-filled, rewritten: branch main\nP\tweb/.git/index\tthe index is damaged and a merge is in progress\nD\n",
        )
        .unwrap();
    assert_eq!(
        r.repaired,
        ["api/.git/HEAD: HEAD was zero-filled, rewritten: branch main"]
    );
    assert_eq!(
        r.damaged,
        ["web/.git/index: the index is damaged and a merge is in progress"]
    );
    assert_eq!(r.removed_count(), 0);
}

#[test]
fn repair_lists_are_bounded() {
    let mut out = String::new();
    for i in 0..MAX_LOCKS + 3 {
        writeln!(out, "R\ta{i}/.git/HEAD\tx").unwrap();
        writeln!(out, "P\ta{i}/.git/index\ty").unwrap();
    }
    out.push_str("D\n");
    let r = parse(&out).unwrap();
    assert_eq!(r.repaired.len(), MAX_LOCKS);
    assert_eq!(r.damaged.len(), MAX_LOCKS);
    assert_eq!(r.repaired[0], "a0/.git/HEAD: x");
}

#[test]
fn a_busy_run_removes_nothing() {
    let r = parse("B\nD\n").unwrap();
    assert!(r.skipped_busy);
    assert_eq!(r.removed_count(), 0);
}

#[test]
fn lists_are_bounded() {
    let mut out = String::new();
    for i in 0..MAX_LOCKS + 7 {
        writeln!(out, "L\ta/.git/{i}.lock").unwrap();
    }
    out.push_str("D\n");
    let r = parse(&out).unwrap();
    assert_eq!(r.removed.len(), MAX_LOCKS);
    assert_eq!(r.more, 7);
    assert_eq!(r.removed_count(), MAX_LOCKS as u64 + 7);
    let long = format!("L\t{}\nD\n", "x".repeat(2000));
    assert_eq!(parse(&long).unwrap().removed[0].chars().count(), MAX_LINE);
}

#[test]
fn odd_output_is_refused() {
    for bad in [
        "",
        "L\ta.lock\n",
        "D\nD\n",
        "D\nL\ta.lock\n",
        "X\nD\n",
        "L\n",
        "D\textra\n",
        "B\tx\nD\n",
        "R\tonly-a-path\nD\n",
        "P\tonly-a-path\nD\n",
    ] {
        assert!(parse(bad).is_err(), "{bad:?} accepted");
    }
}
