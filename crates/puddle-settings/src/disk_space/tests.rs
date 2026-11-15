// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn the_range_is_one_to_ten_thousand() {
    assert!(FreeGb::new(1).is_ok());
    assert!(FreeGb::new(10_000).is_ok());
    assert!(FreeGb::new(0).is_err());
    assert!(FreeGb::new(10_001).is_err());
    assert!(serde_json::from_str::<FreeGb>("0").is_err());
    assert_eq!(serde_json::from_str::<FreeGb>("20").unwrap().gb(), 20);
}

#[test]
fn equal_lines_are_allowed_and_a_stored_pair_out_of_order_is_clamped() {
    let five = FreeGb::new(5).unwrap();
    let nine = FreeGb::new(9).unwrap();
    assert!(DiskSpaceLimits::new(five, five).is_ok());
    let limits = DiskSpaceLimits::clamped(five, nine);
    assert_eq!((limits.warn(), limits.fail()), (five, five));
    let limits = DiskSpaceLimits::clamped(nine, five);
    assert_eq!((limits.warn(), limits.fail()), (nine, five));
}

#[test]
fn a_line_is_crossed_one_byte_under_it() {
    let limits = DiskSpaceLimits::default();
    let at = |gb: u64| gb * GIB;
    assert_eq!(limits.level(at(5)), DiskSpaceLevel::Low);
    assert_eq!(limits.level(at(5) - 1), DiskSpaceLevel::Critical);
    assert_eq!(limits.level(0), DiskSpaceLevel::Critical);
    assert_eq!(limits.level(u64::MAX), DiskSpaceLevel::Ok);
}
