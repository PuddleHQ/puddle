// SPDX-License-Identifier: AGPL-3.0-or-later

use super::*;

#[test]
fn a_signed_pair_is_not_an_escape() {
    assert_eq!(escape_at(b"%+1", 0), None);
    assert_eq!(percent_decoded_bytes(b"%+1"), b"%+1");
    assert_eq!(percent_decoded("%+1"), "%+1");
}

#[test]
fn the_strict_and_the_lenient_reading_agree_on_every_plain_escape() {
    for byte in 0..=u8::MAX {
        for text in [format!("%{byte:02x}"), format!("%{byte:02X}")] {
            let strict = percent_decoded_bytes(text.as_bytes());
            assert_eq!(strict, [byte], "{text}");
            let lenient = percent_decoded(&text);
            assert_eq!(
                lenient.as_bytes(),
                String::from_utf8_lossy(&strict).as_bytes()
            );
        }
    }
}

#[test]
fn the_strict_reading_leaves_what_only_the_lenient_one_decodes() {
    assert_eq!(percent_decoded_bytes(b"%u0041"), b"%u0041");
    assert_eq!(percent_decoded("%u0041"), "A");
    assert_eq!(percent_decoded_bytes(b"%2541"), b"%41");
    assert_eq!(percent_decoded("%2541"), "A");
}
