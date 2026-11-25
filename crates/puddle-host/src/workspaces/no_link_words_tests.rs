// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn a_running_workspace_whose_agent_is_late_is_not_called_stopped_and_the_user_is_not_blamed() {
    let acme = WorkspaceId::new("acme").unwrap();
    assert_eq!(
        no_link_message(&acme, NoLink::NotRunning),
        "acme is not running; start it first"
    );
    let late = no_link_message(&acme, NoLink::NotConnected);
    assert!(late.starts_with("acme is running"));
    assert!(late.contains("open the editor again in a moment"));
}

#[test]
fn an_image_the_editor_cannot_run_in_is_named_not_waited_for() {
    let musl = puddle_front::Unreachable(NEEDS_GLIBC.to_owned());
    let said = editor_login_failure(&musl);
    assert!(said.starts_with(NEEDS_GLIBC));
    assert!(said.contains("desktop VS Code over direct SSH"));
    assert!(!said.contains("yet"));
    let early = editor_login_failure(&puddle_front::Unreachable("not started".to_owned()));
    assert_eq!(
        early,
        "the editor has no password to sign in with yet: not started"
    );
}
