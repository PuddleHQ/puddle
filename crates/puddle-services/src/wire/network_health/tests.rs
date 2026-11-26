// SPDX-License-Identifier: AGPL-3.0-or-later
use serde_json::json;

use super::*;

#[test]
fn the_values_for_a_state_this_build_cannot_name_are_called_unknown_on_the_wire() {
    assert_eq!(
        serde_json::to_value(ProxyDetected::Unknown).unwrap(),
        json!("unknown")
    );
    assert_eq!(
        serde_json::to_value(PacState::Unknown).unwrap(),
        json!("unknown")
    );
    assert_eq!(
        serde_json::to_value(ProxyMode::Unknown).unwrap(),
        json!("unknown")
    );
    assert_eq!(
        serde_json::to_value(SignInResult::Unknown).unwrap(),
        json!("unknown")
    );
    assert_eq!(
        serde_json::to_value(RouteSource::Unknown).unwrap(),
        json!("unknown")
    );
    assert_eq!(
        serde_json::to_value(RootKind::Unknown).unwrap(),
        json!("unknown")
    );
    assert_eq!(
        serde_json::from_value::<RootKind>(json!("unknown")).unwrap(),
        RootKind::Unknown
    );
}
