// SPDX-License-Identifier: AGPL-3.0-or-later
#![allow(clippy::unwrap_used, reason = "test code: a panic is how a test fails")]
//! The login-session read, against a fake `loginctl` that answers the way `loginctl show-user
//! --property=Display --value` and `show-session --property=Type --value` do.
#![cfg(target_os = "linux")]

use puddle_secrets::{LoginSession, ToolPaths, login_session};

use crate::common::Fakes;

async fn session_with(behaviour: &str) -> LoginSession {
    let fakes = Fakes::new();
    let tool = fakes.install("loginctl", behaviour);
    login_session(&ToolPaths::default().with_loginctl(Some(tool))).await
}

#[tokio::test]
async fn a_wayland_display_session_is_graphical() {
    let behaviour = "[show-user]\nstdout=c3\\n\n[show-session]\nstdout=wayland\\n\n";
    assert_eq!(session_with(behaviour).await, LoginSession::Graphical);
}

#[tokio::test]
async fn x11_and_mir_are_graphical_too() {
    for kind in ["x11", "mir"] {
        let behaviour = format!("[show-user]\nstdout=2\\n\n[show-session]\nstdout={kind}\\n\n");
        assert_eq!(session_with(&behaviour).await, LoginSession::Graphical);
    }
}

#[tokio::test]
async fn a_tty_display_session_is_not_graphical() {
    // Measured on the WSL host: `Display` names session c1, of type tty.
    let behaviour = "[show-user]\nstdout=c1\\n\n[show-session]\nstdout=tty\\n\n";
    assert_eq!(session_with(behaviour).await, LoginSession::NotGraphical);
}

#[tokio::test]
async fn no_display_session_is_not_graphical() {
    let behaviour = "[show-user]\nstdout=\\n\n";
    assert_eq!(session_with(behaviour).await, LoginSession::NotGraphical);
}

#[tokio::test]
async fn a_refused_question_or_a_session_id_that_is_not_one_is_unknown() {
    assert_eq!(
        session_with("[show-user]\nexit=1\n").await,
        LoginSession::Unknown
    );
    assert_eq!(
        session_with("[show-user]\nstdout=c1\\n\n[show-session]\nexit=1\n").await,
        LoginSession::Unknown
    );
    assert_eq!(
        session_with("[show-user]\nstdout=--help\\n\n").await,
        LoginSession::Unknown
    );
}

#[tokio::test]
async fn no_loginctl_is_unknown() {
    assert_eq!(
        login_session(&ToolPaths::default()).await,
        LoginSession::Unknown
    );
}

#[test]
fn the_user_id_is_the_one_the_kernel_reports_for_this_process() {
    let status = std::fs::read_to_string("/proc/self/status").unwrap();
    let real: u32 = status
        .lines()
        .find_map(|line| line.strip_prefix("Uid:"))
        .and_then(|ids| ids.split_whitespace().next())
        .and_then(|id| id.parse().ok())
        .unwrap();
    assert_eq!(puddle_secrets::login_user_id(), Some(real));
}
