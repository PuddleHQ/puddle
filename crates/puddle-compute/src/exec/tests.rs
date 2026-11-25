// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn debug_shows_the_length_of_stdin_never_its_bytes() {
    let r = ExecRequest::sh("cat").with_stdin(b"CANARY-KEY".to_vec());
    let shown = format!("{r:?}");
    assert!(
        shown.contains("<10 bytes>") && !shown.contains("CANARY"),
        "{shown}"
    );
}

#[test]
fn request_builders() {
    let mut env = GuestEnv::new();
    env.set("A", "1").unwrap();
    let r = ExecRequest::new("printenv", ["A"])
        .with_env(&env)
        .in_dir(GuestPath::new("/tmp").unwrap())
        .with_stdin(b"in".to_vec())
        .as_user("root");
    assert_eq!(r.program, "printenv");
    assert_eq!(r.args, ["A"]);
    assert_eq!(r.env.get("A"), Some("1"));
    assert_eq!(r.cwd.as_ref().map(GuestPath::as_str), Some("/tmp"));
    assert_eq!(r.stdin, b"in");
    assert_eq!(r.user.as_deref(), Some("root"));
    assert_eq!(r.timeout, ExecRequest::DEFAULT_TIMEOUT);
    assert_eq!(
        r.with_timeout(Duration::from_secs(1)).timeout,
        Duration::from_secs(1)
    );
}

#[test]
fn exit_status_success_and_signal() {
    assert!(ExitStatus::code(0).success());
    assert!(!ExitStatus::code(3).success());
    assert!(!ExitStatus::signalled().success());
    assert_eq!(ExitStatus::signalled().code, -1);
}

#[test]
fn output_text_is_lossy() {
    let o = ExecOutput::new(0, b"ok\xff".to_vec(), b"err".to_vec());
    assert_eq!(o.stdout_text(), "ok\u{fffd}");
    assert_eq!(o.stderr_text(), "err");
    assert!(o.status.success());
}

#[test]
fn a_request_is_capped_by_default_and_the_cap_can_change() {
    let r = ExecRequest::sh("true");
    assert_eq!(r.max_output, Some(ExecRequest::DEFAULT_MAX_OUTPUT));
    assert_eq!(r.clone().with_max_output(7).max_output, Some(7));
    assert_eq!(r.uncapped().max_output, None);
}

#[test]
fn capped_cuts_each_stream_and_says_which() {
    let out = ExecOutput::new(0, vec![b'o'; 10], b"ok".to_vec()).capped(Some(4));
    assert_eq!((out.stdout.len(), out.stdout_truncated), (4, true));
    assert_eq!((out.stderr.len(), out.stderr_truncated), (2, false));
    let whole = ExecOutput::new(0, vec![b'o'; 10], "").capped(None);
    assert_eq!((whole.stdout.len(), whole.stdout_truncated), (10, false));
}
