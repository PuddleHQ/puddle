// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use std::collections::HashSet;

#[test]
fn names_are_lowercase_hex_digits_two_per_byte() {
    let n = random_name::<16>().unwrap();
    assert_eq!(n.len(), 32);
    assert_eq!(random_name::<8>().unwrap().len(), 16);
    assert!(
        n.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "{n}"
    );
}

#[test]
fn names_do_not_repeat() {
    let names: HashSet<String> = (0..1000).map(|_| random_name::<8>().unwrap()).collect();
    assert_eq!(names.len(), 1000);
}

#[test]
fn hex_encodes_every_nibble() {
    assert_eq!(hex(&[0x00, 0x0f, 0xa5, 0xff]), "000fa5ff");
    assert_eq!(hex(&[]), "");
}
