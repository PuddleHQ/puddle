// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn the_header_name_is_lower_case_for_the_proxy_comparison() {
    assert_eq!(PROGRAM_HEADER, PROGRAM_HEADER.to_ascii_lowercase());
}
