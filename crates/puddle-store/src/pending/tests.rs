// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn states_round_trip_through_text() {
    for state in [
        PendingState::Requested,
        PendingState::Allowed,
        PendingState::Denied,
        PendingState::Expired,
    ] {
        assert_eq!(PendingState::parse(state.as_str()), Some(state));
    }
    assert_eq!(PendingState::parse("open"), None);
}

#[test]
fn r15_resolution_defaults_are_narrowest() {
    let allow = Resolution::allow();
    assert_eq!(allow.effect, Effect::Allow);
    assert_eq!(allow.scope, ScopeChoice::Workspace);
    assert_eq!(allow.pattern, PatternChoice::Exact);
    assert_eq!(allow.expires_in, None);
    assert_eq!(Resolution::deny().effect, Effect::Deny);
}
