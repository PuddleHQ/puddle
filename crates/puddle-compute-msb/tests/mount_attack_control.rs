// SPDX-License-Identifier: GPL-3.0-or-later
//! The negative control of the boot-mounts hostile test (`vm_hostile_boot.rs`, the HG-23 case): the
//! judgement that test relies on must fail when the share is writable, or a green VM run would
//! prove nothing. These tests need no VM, so they run in every gate (the VM test's binary is
//! `vm_*` and runs only under the VM profile).
//!
//! - The judge accepts exactly the refusal the mounts promise, and flags any one operation that
//!   behaves differently, in both states of the guest's flag and with or without a running binary.
//! - It flags every kind of change to the host's files: content, length, modification time, mode,
//!   links, a new or a missing entry.
//! - The real attack script, run against an ordinary writable file (what a writable share would
//!   be), makes the judgement fail on the writes and on the host's files.
#![expect(
    clippy::unwrap_used,
    reason = "test helpers: a failed step fails the test"
)]

mod support;

use std::fs::{File, FileTimes};
use std::path::Path;
use std::time::{Duration, SystemTime};

use support::mount_attack::{HostDir, OPS, Report, expected, judge, judge_outcomes};

/// What the mounts promise for a file in `state`.
fn promised(state: &str, running: bool) -> Report {
    let outcomes: Vec<(&str, &str)> = OPS
        .iter()
        .map(|op| (*op, expected(state == "ro", op, running)))
        .collect();
    Report::of(state, running, &outcomes)
}

/// Three files like the boot mounts' sources, in a folder of their own.
fn folder() -> (tempfile::TempDir, [&'static str; 3]) {
    let dir = tempfile::tempdir().unwrap();
    let names = ["boot.sh", "agent-supervise.sh", "puddle-agent"];
    for n in names {
        std::fs::write(dir.path().join(n), format!("content of {n}\n")).unwrap();
    }
    (dir, names)
}

fn rewind(path: &Path, to: SystemTime) {
    let file = File::options().write(true).open(path).unwrap();
    file.set_times(FileTimes::new().set_modified(to)).unwrap();
}

#[test]
fn the_promised_refusal_passes_in_every_state() {
    let (dir, names) = folder();
    let host = HostDir::take(dir.path(), &names);
    for state in ["ro", "rw"] {
        for running in [false, true] {
            let failures = judge("t", &promised(state, running), &host, &host);
            assert!(failures.is_empty(), "{state} {running}: {failures:?}");
        }
    }
}

#[test]
fn an_operation_that_behaves_differently_is_flagged_by_name_and_alone() {
    let (dir, names) = folder();
    let host = HostDir::take(dir.path(), &names);
    for state in ["ro", "rw"] {
        for running in [false, true] {
            for op in OPS {
                // The write went through (or a different error came back).
                for wrong in ["ok", "EIO"] {
                    let mut report = promised(state, running);
                    if report.outcomes[op] == wrong {
                        continue;
                    }
                    report.outcomes.insert(op.to_owned(), wrong.to_owned());
                    let failures = judge("t", &report, &host, &host);
                    assert_eq!(
                        failures.len(),
                        1,
                        "{state} {running} {op}={wrong}: {failures:?}"
                    );
                    assert!(
                        failures[0].contains(&format!("{op} gave {wrong}")),
                        "{failures:?}"
                    );
                }
                // No outcome at all (the script died before it).
                let mut report = promised(state, running);
                report.outcomes.remove(op);
                assert_eq!(
                    judge_outcomes("t", &report).len(),
                    1,
                    "{state} {op} missing"
                );
            }
        }
    }
}

#[test]
fn a_text_busy_answer_is_only_right_where_a_running_binary_makes_it() {
    // The cells where ETXTBSY is expected are exactly: flag cleared, binary running, a write open.
    for (state, running, op, busy) in [
        ("rw", true, "append", true),
        ("rw", true, "truncate", true),
        ("rw", true, "chmod", false),
        ("rw", false, "append", false),
        ("ro", true, "append", false),
    ] {
        assert_eq!(
            expected(state == "ro", op, running) == "ETXTBSY",
            busy,
            "{state} {running} {op}"
        );
    }
}

#[test]
fn a_state_that_is_not_a_mount_is_flagged() {
    let failures = judge_outcomes("t", &promised("none", false));
    assert!(
        failures.iter().any(|f| f.contains("not a mount")),
        "{failures:?}"
    );
}

#[test]
fn every_kind_of_change_to_the_host_files_is_flagged() {
    let (dir, names) = folder();
    let path = |n: &str| dir.path().join(n);
    let before = HostDir::take(dir.path(), &names);
    assert_eq!(before.differences(&before), Vec::<String>::new());
    let original_time = std::fs::metadata(path("boot.sh"))
        .unwrap()
        .modified()
        .unwrap();
    let later = original_time + Duration::from_secs(60);

    let changed = |what: &str, mutate: &dyn Fn()| {
        mutate();
        let now = HostDir::take(dir.path(), &names);
        let found = before.differences(&now);
        assert!(
            found.iter().any(|f| f.contains(what)),
            "{what} not flagged: {found:?}"
        );
    };
    // Same length and same time, other bytes: only the hash says so.
    changed("sha256", &|| {
        std::fs::write(path("boot.sh"), "CONTENT of boot.sh\n").unwrap();
        rewind(&path("boot.sh"), original_time);
    });
    changed("length", &|| {
        std::fs::write(path("boot.sh"), "content of boot.sh\nmore\n").unwrap();
    });
    std::fs::write(path("boot.sh"), "content of boot.sh\n").unwrap();
    rewind(&path("boot.sh"), original_time);
    changed("modification time", &|| rewind(&path("boot.sh"), later));
    rewind(&path("boot.sh"), original_time);
    changed("appeared in the host folder", &|| {
        std::fs::write(path("new-file"), "x").unwrap();
    });
    std::fs::remove_file(path("new-file")).unwrap();
    changed("gone", &|| {
        std::fs::remove_file(path("puddle-agent")).unwrap();
    });
    std::fs::write(path("puddle-agent"), "content of puddle-agent\n").unwrap();
    rewind(
        &path("puddle-agent"),
        before.files["puddle-agent"].as_ref().unwrap().modified,
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        changed("mode", &|| {
            std::fs::set_permissions(path("agent-supervise.sh"), PermissionsExt::from_mode(0o755))
                .unwrap();
        });
        changed("link count", &|| {
            std::fs::hard_link(path("agent-supervise.sh"), dir.path().join("hard")).unwrap();
        });
    }
}

/// The real attack script on an ordinary writable file: what a writable share would let root do.
/// The judgement must fail on the writes and on the host's files, and the script itself must run
/// (a script that broke would also "fail" for the wrong reason, so the report is checked whole).
#[cfg(unix)]
#[test]
fn a_writable_share_fails_the_judgement() {
    use support::mount_attack::ATTACK;

    let (dir, names) = folder();
    let target = dir.path().join("boot.sh");
    let before = HostDir::take(dir.path(), &names);
    let run = std::process::Command::new("perl")
        .args(["-e", ATTACK])
        .arg(&target)
        .output()
        .expect("perl must be installed (the attack script is perl)");
    assert!(
        run.status.success(),
        "attack script: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    let report = Report::parse(&String::from_utf8_lossy(&run.stdout));
    assert_eq!(
        report.outcomes.len(),
        OPS.len(),
        "every operation reports: {report:?}"
    );
    let after = HostDir::take(dir.path(), &names);
    let failures = judge("control", &report, &before, &after);
    let text = failures.join("\n");
    // Every write path that is not refused with its own errno is named (`unlink` finds the
    // file already renamed away here, so its answer is ENOENT) ...
    for op in [
        "open_write",
        "append",
        "o_trunc",
        "truncate",
        "chmod",
        "utimes",
        "link",
        "rename_over",
        "rename_away",
        "unlink",
    ] {
        assert!(
            failures.iter().any(|f| f.contains(&format!("{op} gave "))),
            "{op} not flagged:\n{text}"
        );
    }
    // ... and so is what became of the host's file.
    assert!(text.contains("boot.sh: gone"), "{text}");
    assert!(
        text.contains("boot.sh: on the host") || text.contains("on the host, boot.sh"),
        "{text}"
    );
}
