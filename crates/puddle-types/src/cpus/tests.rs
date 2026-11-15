// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn only_zero_and_the_storage_width_are_rejected() {
    assert_eq!(CpuCount::new(1).unwrap(), CpuCount::MIN);
    assert_eq!(CpuCount::new(255).unwrap(), CpuCount::MAX);
    assert!(CpuCount::new(0).is_err());
    assert!(CpuCount::new(256).is_err());
    // The old fixed limit of 32 is gone.
    assert_eq!(CpuCount::new(33).unwrap().get(), 33);
    let err = CpuCount::new(256).unwrap_err();
    assert!(err.to_string().contains("at most 255"), "{err}");
}

#[test]
fn display_and_conversions() {
    assert_eq!(CpuCount::new(4).unwrap().to_string(), "4");
    assert_eq!(u32::from(CpuCount::MAX), 255);
    assert_eq!(CpuCount::try_from(2).unwrap().get(), 2);
    assert_eq!(CpuCount::default(), CpuCount::DEFAULT);
}

#[test]
fn serde_validates_on_the_way_in() {
    assert_eq!(serde_json::to_string(&CpuCount::DEFAULT).unwrap(), "4");
    assert_eq!(serde_json::from_str::<CpuCount>("4").unwrap().get(), 4);
    assert_eq!(serde_json::from_str::<CpuCount>("64").unwrap().get(), 64);
    assert!(serde_json::from_str::<CpuCount>("0").is_err());
    assert!(serde_json::from_str::<CpuCount>("300").is_err());
}

#[test]
fn the_default_is_four_or_half_the_cores_whichever_is_less_and_at_least_one() {
    let default_on = |cores| CpuCount::default_for_host(cores).get();
    assert_eq!(default_on(0), 1);
    assert_eq!(default_on(1), 1);
    assert_eq!(default_on(2), 1);
    assert_eq!(default_on(3), 1);
    assert_eq!(default_on(4), 2);
    assert_eq!(default_on(6), 3);
    assert_eq!(default_on(8), 4);
    assert_eq!(default_on(16), 4);
    assert_eq!(default_on(u32::MAX), 4);
}

#[test]
fn a_host_core_count_clamps_into_the_type() {
    assert_eq!(CpuCount::clamped(0).get(), 1);
    assert_eq!(CpuCount::clamped(12).get(), 12);
    assert_eq!(CpuCount::clamped(1000).get(), 255);
}
