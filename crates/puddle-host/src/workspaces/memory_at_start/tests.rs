// SPDX-License-Identifier: AGPL-3.0-or-later
use super::fits;

#[test]
fn a_start_fits_up_to_exactly_what_is_available_and_an_unknown_host_fits_everything() {
    let gib = 1024_u64 * 1024 * 1024;
    assert!(fits(8 * 1024, Some(8 * gib)));
    assert!(!fits(8 * 1024 + 1, Some(8 * gib)));
    assert!(!fits(256, Some(0)));
    assert!(fits(1024 * 1024, None));
}
