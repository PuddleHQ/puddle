// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn bounds_are_inclusive() {
    assert_eq!(MemoryMib::new(256).unwrap(), MemoryMib::MIN);
    assert_eq!(MemoryMib::new(1024 * 1024).unwrap(), MemoryMib::MAX);
    assert!(MemoryMib::new(255).is_err());
    let err = MemoryMib::new(1024 * 1024 + 1).unwrap_err();
    assert!(
        err.to_string().contains("between 256 and 1048576 MiB"),
        "{err}"
    );
}

#[test]
fn the_default_is_eight_gib() {
    assert_eq!(MemoryMib::default().get(), 8192);
}

#[test]
fn display_and_conversions() {
    assert_eq!(MemoryMib::DEFAULT.to_string(), "8192 MiB");
    assert_eq!(u32::from(MemoryMib::MIN), 256);
    assert_eq!(MemoryMib::try_from(512).unwrap().get(), 512);
}

#[test]
fn serde_validates_on_the_way_in() {
    assert_eq!(serde_json::to_string(&MemoryMib::DEFAULT).unwrap(), "8192");
    assert_eq!(
        serde_json::from_str::<MemoryMib>("4096").unwrap().get(),
        4096
    );
    assert!(serde_json::from_str::<MemoryMib>("1").is_err());
}
