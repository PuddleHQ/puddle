// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

fn out(code: i32, stderr: &str) -> ExecOutput {
    ExecOutput::new(code, Vec::new(), stderr.as_bytes())
}

#[test]
fn no_paths_trims_every_mounted_filesystem() {
    let r = trim_request(&[], Duration::from_secs(7));
    assert_eq!(r.program, "sh");
    assert_eq!(r.args, ["-c", TRIM_ALL]);
    assert!(TRIM_ALL.starts_with("fstrim -a -v"), "util-linux first");
    assert_eq!(r.user.as_deref(), Some("root"));
    assert_eq!(r.timeout, Duration::from_secs(7));
}

#[test]
fn paths_are_arguments_not_script_text() {
    let paths = [
        GuestPath::new("/workspaces/a").unwrap(),
        GuestPath::new("/srv/$(reboot)").unwrap(),
    ];
    let r = trim_request(&paths, Duration::from_secs(1));
    assert_eq!(r.program, "sh");
    assert_eq!(r.args[0], "-c");
    assert!(!r.args[1].contains("/workspaces"));
    assert_eq!(&r.args[3..], ["/workspaces/a", "/srv/$(reboot)"]);
}

#[test]
fn outcome_keeps_the_code_and_the_end_of_stderr() {
    assert_eq!(TrimOutcome::from_output(&out(0, "")), TrimOutcome::Trimmed);
    let long = format!("{}tail", "x".repeat(1000));
    let TrimOutcome::Failed { code, stderr } = TrimOutcome::from_output(&out(32, &long)) else {
        panic!("want Failed");
    };
    assert_eq!(code, 32);
    assert_eq!(stderr.chars().count(), STDERR_TAIL);
    assert!(stderr.ends_with("tail"));
}
