// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn system_clock_is_after_2026() {
    assert!(SystemClock.now_ms() > 1_767_225_600_000);
}

#[test]
fn manual_clock_moves_only_when_told() {
    let clock = ManualClock::new(5);
    assert_eq!(clock.now_ms(), 5);
    clock.advance(10);
    assert_eq!(clock.now_ms(), 15);
    clock.set(2);
    assert_eq!(clock.now_ms(), 2);
}
