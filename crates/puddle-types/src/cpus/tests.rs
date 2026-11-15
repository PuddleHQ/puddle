// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn bounds_are_inclusive() {
    assert_eq!(CpuCount::new(1).unwrap(), CpuCount::MIN);
    assert_eq!(CpuCount::new(32).unwrap(), CpuCount::MAX);
    assert!(CpuCount::new(0).is_err());
    assert!(CpuCount::new(256).is_err());
    let err = CpuCount::new(33).unwrap_err();
    assert!(err.to_string().contains("between 1 and 32"), "{err}");
}

#[test]
fn display_and_conversions() {
    assert_eq!(CpuCount::new(4).unwrap().to_string(), "4");
    assert_eq!(u32::from(CpuCount::MAX), 32);
    assert_eq!(CpuCount::try_from(2).unwrap().get(), 2);
    assert_eq!(CpuCount::default(), CpuCount::DEFAULT);
}

#[test]
fn serde_validates_on_the_way_in() {
    assert_eq!(serde_json::to_string(&CpuCount::DEFAULT).unwrap(), "1");
    assert_eq!(serde_json::from_str::<CpuCount>("4").unwrap().get(), 4);
    assert!(serde_json::from_str::<CpuCount>("0").is_err());
    assert!(serde_json::from_str::<CpuCount>("300").is_err());
}
