// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn every_console_event_is_a_shutdown_cause() {
    assert_eq!(cause(CTRL_C_EVENT), Some(ShutdownCause::Interrupt));
    assert_eq!(cause(CTRL_BREAK_EVENT), Some(ShutdownCause::Break));
    assert_eq!(cause(CTRL_CLOSE_EVENT), Some(ShutdownCause::ConsoleClose));
    assert_eq!(cause(CTRL_LOGOFF_EVENT), Some(ShutdownCause::Logoff));
    assert_eq!(
        cause(CTRL_SHUTDOWN_EVENT),
        Some(ShutdownCause::SystemShutdown)
    );
    assert_eq!(cause(99), None);
}

#[test]
fn without_a_front_events_go_to_the_default_handler() {
    assert!(!on_console_event(CTRL_C_EVENT));
}
