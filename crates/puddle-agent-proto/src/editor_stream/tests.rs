// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn a_request_is_the_preamble_and_one_word() {
    assert_eq!(request(Request::Connect), b"\0puddle-editor/1\nconnect\n");
    assert_eq!(request(Request::Password), b"\0puddle-editor/1\npassword\n");
    assert!(PREAMBLE.len() <= crate::kind::MAX_PREAMBLE);
}

#[test]
fn only_the_two_words_parse() {
    assert_eq!(Request::parse(b"connect"), Ok(Request::Connect));
    for bad in [
        &b""[..],
        b"\n",
        b"Connect\n",
        b"connect 41337\n",
        b"41337\n",
        b"password\r\n",
    ] {
        assert_eq!(Request::parse(bad), Err(BadRequest), "{bad:?}");
    }
}

#[test]
fn a_password_is_hex_and_fits_a_line() {
    let hex = puddle_types::fixture::suffix();
    assert!(is_password(&hex.repeat(4)));
    let too_long = hex.repeat(MAX_ANSWER_LINE);
    for bad in [
        String::new(),
        format!("{hex}!"),
        format!("{hex} {hex}"),
        too_long,
    ] {
        assert!(!is_password(&bad), "{bad:?}");
    }
    assert_eq!(password_line(hex), format!("password {hex}\n").into_bytes());
}
