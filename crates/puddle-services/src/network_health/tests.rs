// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn a_pac_that_answered_or_failed_is_reported_whatever_was_detected() {
    for detected in [
        Detected::Pac,
        Detected::Wpad,
        Detected::Static,
        Detected::Direct,
    ] {
        for auto_detect in [false, true] {
            assert_eq!(
                pac_state(detected, Some(false), auto_detect),
                PacState::Unreachable
            );
            assert_eq!(
                pac_state(detected, Some(true), auto_detect),
                PacState::Answering
            );
        }
    }
}

#[test]
fn a_configured_pac_nobody_asked_yet_is_not_asked() {
    assert_eq!(pac_state(Detected::Pac, None, false), PacState::NotAsked);
    assert_eq!(pac_state(Detected::Wpad, None, false), PacState::NotAsked);
    assert_eq!(pac_state(Detected::Static, None, true), PacState::NotAsked);
    assert_eq!(pac_state(Detected::Env, None, true), PacState::NotAsked);
    assert_eq!(pac_state(Detected::Direct, None, true), PacState::NotAsked);
}

#[test]
fn a_fixed_or_absent_proxy_without_automatic_detection_does_not_use_a_pac() {
    for detected in [Detected::Static, Detected::Env, Detected::Direct] {
        assert_eq!(pac_state(detected, None, false), PacState::NotUsed);
    }
}
