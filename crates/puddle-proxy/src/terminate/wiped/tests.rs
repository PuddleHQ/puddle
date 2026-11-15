// SPDX-License-Identifier: AGPL-3.0-or-later
use std::io::Write as _;

use super::*;

#[test]
fn it_grows_and_keeps_every_byte() {
    let mut w = Wiped::new();
    let data: Vec<u8> = (0..10_000u32).map(|i| (i % 251) as u8).collect();
    for chunk in data.chunks(777) {
        w.write_all(chunk).unwrap();
    }
    w.flush().unwrap();
    assert_eq!(w.as_ref(), &data[..]);
    assert_eq!(&w.into_bytes()[..], &data[..]);
}

#[test]
fn bytes_that_exactly_fill_the_buffer_do_not_grow_it() {
    let mut w = Wiped::with_capacity(8);
    w.extend_from_slice(b"abcd");
    w.extend_from_slice(b"efgh");
    assert_eq!(w.0.capacity(), 8);
    w.extend_from_slice(b"i");
    assert_eq!(w.as_ref(), b"abcdefghi");
    assert_eq!(w.0.capacity(), 16);
}

#[test]
fn a_copy_is_exactly_as_big_as_its_source() {
    let w = Wiped::copy_of(b"abc");
    assert_eq!(w.len(), 3);
    assert_eq!(w.0.capacity(), 3);
}
