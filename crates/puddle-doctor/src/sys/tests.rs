// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[cfg(target_arch = "x86_64")]
#[test]
fn vendor_names() {
    assert_eq!(vendor_name(b"Microsoft Hv"), "Microsoft Hv");
    assert_eq!(vendor_name(b"KVMKVMKVM\0\0\0"), "KVMKVMKVM");
    assert_eq!(vendor_name(b"\0\0\0\0"), "unknown");
    assert_eq!(vendor_name(b"\x01\x02"), "unknown");
}

#[test]
fn vendor_probe_does_not_panic() {
    let _any = hypervisor_vendor();
}
