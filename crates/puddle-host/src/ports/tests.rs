// SPDX-License-Identifier: AGPL-3.0-or-later
use puddle_ports::PortOwner;

use super::*;

#[test]
fn every_refusal_keeps_its_words_and_lands_in_the_service_error_that_names_its_kind() {
    let taken = PortsError::Taken {
        host_port: 80,
        owner: PortOwner::Program(None),
    };
    let cannot = PortsError::Cannot {
        host_port: 80,
        reason: "denied".into(),
    };
    for (err, expected) in [
        (PortsError::NotRunning, "NotRunning"),
        (PortsError::BadPort, "Invalid"),
        (taken, "Taken"),
        (cannot, "Taken"),
        (PortsError::NotForwarded { guest_port: 1 }, "NotForwarded"),
        (PortsError::Pins("locked".into()), "Failed"),
    ] {
        let mapped = refusal(&err);
        assert!(
            format!("{mapped:?}").starts_with(expected),
            "{err}: {mapped:?}"
        );
        assert_eq!(mapped.to_string(), err.to_string());
    }
}
