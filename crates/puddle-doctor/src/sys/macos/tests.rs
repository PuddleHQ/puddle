// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn the_hypervisor_is_unsupported_for_now() {
    let facts = hypervisor();
    assert_eq!(facts.api, HypervisorApi::Unsupported);
    assert_eq!(facts.firmware_virtualization, None);
}
