// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn the_linked_stack_is_a_number() {
    assert_eq!(linked_stack_bytes(), 8 * 1024 * 1024);
}

#[cfg(windows)]
#[test]
fn a_thread_with_too_little_stack_is_told_why_instead_of_overflowing() {
    let message = std::thread::Builder::new()
        .stack_size(512 * 1024)
        .spawn(check_main_thread)
        .unwrap()
        .join()
        .unwrap()
        .unwrap_err();
    assert!(message.contains("/STACK"), "{message}");
}

#[cfg(windows)]
#[test]
fn a_thread_with_the_linked_stack_passes() {
    std::thread::Builder::new()
        .stack_size(linked_stack_bytes())
        .spawn(check_main_thread)
        .unwrap()
        .join()
        .unwrap()
        .unwrap();
}

#[cfg(not(windows))]
#[test]
fn elsewhere_the_check_passes() {
    check_main_thread().unwrap();
}

#[test]
fn a_failed_check_is_named_on_stderr_exits_2_and_never_serves() {
    let mut stderr = Vec::new();
    let code = launch(
        || Err("too small".into()),
        || panic!("must not serve"),
        &mut stderr,
    );
    assert_eq!(code, ExitCode::from(2));
    assert_eq!(
        String::from_utf8(stderr).unwrap(),
        "puddle-ui-fixture: too small\n"
    );
}

#[test]
fn a_passing_check_serves_and_returns_its_exit_code() {
    let mut stderr = Vec::new();
    let code = launch(|| Ok(()), || ExitCode::from(7), &mut stderr);
    assert_eq!(code, ExitCode::from(7));
    assert_eq!(stderr.len(), 0);
}

#[test]
fn a_finished_run_is_success_or_2_with_its_message() {
    let mut stderr = Vec::new();
    assert_eq!(finish(Ok(()), &mut stderr), ExitCode::SUCCESS);
    assert_eq!(stderr.len(), 0);
    assert_eq!(finish(Err("boom".into()), &mut stderr), ExitCode::from(2));
    assert_eq!(
        String::from_utf8(stderr).unwrap(),
        "puddle-ui-fixture: boom\n"
    );
}
