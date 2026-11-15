// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

fn shell(script: &str) -> Command {
    if cfg!(windows) {
        let mut c = Command::new("cmd");
        c.args(["/d", "/c", script]);
        c
    } else {
        let mut c = Command::new("sh");
        c.args(["-c", script]);
        c
    }
}

#[test]
fn exit_code_first_stdout_line_and_stderr_tail() {
    let script = if cfg!(windows) {
        "echo first& echo second& (for /l %i in (1,1,10) do @echo err%i 1>&2)& exit 3"
    } else {
        "echo first; echo second; for i in 1 2 3 4 5 6 7 8 9 10; do echo err$i >&2; done; exit 3"
    };
    let ProcessOutcome::Exited {
        code,
        stdout_first_line,
        stderr_tail,
        ..
    } = run(&mut shell(script), Duration::from_secs(20))
    else {
        panic!("expected an exit");
    };
    assert_eq!(code, Some(3));
    assert_eq!(stdout_first_line, "first");
    let lines: Vec<&str> = stderr_tail.lines().collect();
    assert_eq!(lines.len(), TAIL_LINES);
    assert_eq!(lines.last().map(|l| l.trim()), Some("err10"));
}

#[test]
fn a_hanging_program_is_killed_at_the_limit() {
    let script = if cfg!(windows) {
        "ping -n 30 127.0.0.1 >nul"
    } else {
        "sleep 30"
    };
    let start = Instant::now();
    let outcome = run(&mut shell(script), Duration::from_millis(300));
    assert_eq!(
        outcome,
        ProcessOutcome::TimedOut {
            after: Duration::from_millis(300)
        }
    );
    // flake-ok: the shell's `sleep` runs 30 s, so a run that waited for it (or for its pipes)
    // ends at 30 s; one killed at the limit takes 0.3 s (0.34 s measured under load).
    assert!(start.elapsed() < Duration::from_secs(10));
}

#[test]
fn a_missing_program_fails_to_spawn_with_its_os_error() {
    let outcome = run(
        &mut Command::new(std::path::absolute("puddle-doctor-no-such-program").unwrap()),
        Duration::from_secs(5),
    );
    let ProcessOutcome::SpawnFailed { os_error, detail } = outcome else {
        panic!("expected a spawn failure");
    };
    assert_eq!(os_error, Some(2));
    assert_ne!(detail, "");
}

#[test]
fn clean_strips_escapes_and_controls() {
    assert_eq!(
        clean("\u{1b}[1;31merror\u{1b}[0m: x\r\n\u{7}  \u{1b}[2K→ y\n"),
        "error: x\n  → y\n"
    );
    assert_eq!(clean("tab\tkept"), "tab\tkept");
    assert_eq!(clean("lone \u{1b}x"), "lone x");
}

#[test]
fn first_line_skips_blank_lines() {
    assert_eq!(first_line("\n  \n msb 0.7.7 \nnext"), "msb 0.7.7");
    assert_eq!(first_line(""), "");
}

#[test]
fn tail_keeps_the_last_non_blank_lines() {
    assert_eq!(tail("a\n\nb\nc\n\nd\n", 2), "c\nd");
    assert_eq!(tail("a\n", 5), "a");
    assert_eq!(tail("", 5), "");
}
