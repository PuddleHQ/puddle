// SPDX-License-Identifier: AGPL-3.0-or-later
use std::fmt::Write as _;

use super::*;

#[test]
fn a_full_output_parses_into_findings() {
    let out = "R\tapi\nU\t M src/main.rs\nU\t?? notes.txt\nC\tabc1234 wip\nS\tstash@{0}: WIP on main: x\n\
                   R\tweb\nO\tscratch.txt\nE\tlib\tgit status failed: boom\nM\tO\t3\nD\n";
    let f = parse(out).unwrap();
    assert_eq!(f.repos.len(), 2);
    let api = &f.repos[0];
    assert_eq!(api.dir, "api");
    assert_eq!(api.uncommitted.items, [" M src/main.rs", "?? notes.txt"]);
    assert_eq!(api.unpushed.items, ["abc1234 wip"]);
    assert_eq!(api.stashes.items, ["stash@{0}: WIP on main: x"]);
    assert!(!api.is_clean());
    assert!(f.repos[1].is_clean());
    assert_eq!(f.other.items, ["scratch.txt"]);
    assert_eq!(f.other.more, 3);
    assert_eq!(f.other.total(), 4);
    assert_eq!(f.errors, ["lib: git status failed: boom"]);
    assert!(!f.is_clean());
}

#[test]
fn an_empty_volume_is_clean() {
    let f = parse("D\n").unwrap();
    assert!(f.is_clean());
    let f = parse("R\tapi\nD\n").unwrap();
    assert!(f.is_clean());
}

#[test]
fn each_kind_alone_makes_it_unclean() {
    for out in [
        "R\ta\nU\t?? x\nD\n",
        "R\ta\nC\tabc x\nD\n",
        "R\ta\nS\tstash@{0}: x\nD\n",
        "R\ta\nM\tU\t5\nD\n",
        "O\tx\nD\n",
        "E\ta\tgit is not installed in the image\nD\n",
    ] {
        assert!(!parse(out).unwrap().is_clean(), "{out:?}");
    }
}

#[test]
fn malformed_or_unfinished_output_is_refused() {
    for (out, want) in [
        ("", "did not finish"),
        ("R\ta\nU\t?? x\n", "did not finish"),
        ("U\t?? x\nD\n", "before any checkout"),
        ("X\ty\nD\n", "unexpected output line"),
        ("R\nD\n", "unexpected output line"),
        ("M\tU\t1\nD\n", "bad count line"),
        ("R\ta\nM\tQ\t1\nD\n", "bad count line"),
        ("R\ta\nM\tU\tmany\nD\n", "bad count line"),
        ("D\nO\tlate\n", "after the end marker"),
    ] {
        let err = parse(out).unwrap_err();
        assert!(err.contains(want), "{out:?}: {err}");
    }
}

#[test]
fn hostile_output_stays_bounded() {
    let mut out = String::from("R\ta\n");
    for i in 0..(MAX_ITEMS + 50) {
        writeln!(out, "U\t?? {i}{}", "x".repeat(2000)).unwrap();
    }
    out.push_str("M\tU\t18446744073709551615\nD\n");
    let f = parse(&out).unwrap();
    let u = &f.repos[0].uncommitted;
    assert_eq!(u.items.len(), MAX_ITEMS);
    assert!(u.items.iter().all(|i| i.chars().count() <= MAX_LINE));
    assert_eq!(u.more, u64::MAX, "saturates");
    assert_eq!(u.total(), u64::MAX);

    let mut repos = String::new();
    for i in 0..=MAX_REPOS {
        writeln!(repos, "R\tr{i}").unwrap();
    }
    assert!(
        parse(&format!("{repos}D\n"))
            .unwrap_err()
            .contains("checkouts")
    );

    let mut errors = String::new();
    for i in 0..300 {
        writeln!(errors, "E\tr{i}\tx").unwrap();
    }
    assert_eq!(
        parse(&format!("{errors}D\n")).unwrap().errors.len(),
        MAX_ITEMS
    );
}

#[test]
fn the_request_runs_the_script_as_root_on_the_mount_point() {
    let l = Layout::new(&WorkspaceId::new("a").unwrap()).unwrap();
    let r = request(&l);
    assert_eq!(r.program, "sh");
    assert_eq!(
        r.args,
        [
            "-c",
            DELETE_CHECK_SH,
            "puddle-delete-check",
            "/workspaces/a"
        ]
    );
    assert_eq!(r.user.as_deref(), Some("root"));
}

fn report(findings: Findings) -> DeleteReport {
    DeleteReport {
        volume_missing: false,
        workspace: WorkspaceId::new("acme").unwrap(),
        findings,
        checked_in: SandboxName::new("acme").unwrap(),
        removes_sandbox: None,
    }
}

#[test]
fn the_summary_lists_what_would_be_lost() {
    let clean = report(parse("R\tapi\nD\n").unwrap());
    assert_eq!(
        clean.to_string(),
        "workspace acme: nothing unsaved\n  api: clean\n"
    );
    let mut dirty =
        report(parse("R\tapi\nU\t?? a\nM\tU\t2\nC\tabc x\nS\ts\nO\tf\nE\tb\tboom\nD\n").unwrap());
    dirty.removes_sandbox = Some(SandboxName::new("old").unwrap());
    let text = dirty.to_string();
    for want in [
        "deleting loses",
        "uncommitted changes (3):",
        "      ?? a",
        "... and 2 more",
        "unpushed commits (1):",
        "stashes (1):",
        "outside any checkout:",
        "      f",
        "not checked: b: boom",
        "also removes stopped sandbox old",
    ] {
        assert!(text.contains(want), "{want:?} missing from\n{text}");
    }
    let c = dirty.confirm();
    assert_eq!(c.workspace().as_str(), "acme");
}
