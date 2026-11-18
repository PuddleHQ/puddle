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

#[test]
fn a_repeated_interrupt_ends_the_front_but_a_repeated_deadline_event_does_not() {
    for cause in [ShutdownCause::Interrupt, ShutdownCause::Break] {
        assert!(!ends_front_by_default(cause, false), "{cause}: first");
        assert!(ends_front_by_default(cause, true), "{cause}: repeated");
    }
    for cause in [
        ShutdownCause::ConsoleClose,
        ShutdownCause::Logoff,
        ShutdownCause::SystemShutdown,
    ] {
        assert!(!ends_front_by_default(cause, true), "{cause}: repeated");
    }
}
