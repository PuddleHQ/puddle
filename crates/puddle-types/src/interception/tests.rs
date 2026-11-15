// SPDX-License-Identifier: AGPL-3.0-or-later
use super::InterceptionMode;

#[test]
fn every_mode_round_trips_through_its_name() {
    for mode in [
        InterceptionMode::All,
        InterceptionMode::AllExceptCurated,
        InterceptionMode::KnownOnly,
    ] {
        assert_eq!(InterceptionMode::parse(mode.as_str()), Some(mode));
    }
    assert_eq!(InterceptionMode::parse("most"), None);
}

#[test]
fn only_known_only_leaves_unknown_hosts_alone() {
    assert!(InterceptionMode::All.decrypts_unknown_hosts());
    assert!(InterceptionMode::AllExceptCurated.decrypts_unknown_hosts());
    assert!(!InterceptionMode::KnownOnly.decrypts_unknown_hosts());
}
