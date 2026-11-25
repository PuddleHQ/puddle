// SPDX-License-Identifier: AGPL-3.0-or-later

use super::*;

#[test]
fn hex_is_two_lower_case_digits_per_byte() {
    assert_eq!(hex_lower(&[0x00, 0x9f, 0xff, 0x10, 0xab]), "009fff10ab");
    assert_eq!(hex_lower(&[]), "");
}
