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

#[test]
fn a_connect_answer_is_ok_a_refusal_with_its_reason_or_not_an_answer() {
    assert!(connect_answer(&Answer::Ok.to_line()).is_ok());
    let refused = connect_answer(&Answer::Refused("no editor here".into()).to_line());
    assert!(
        matches!(refused, Err(OpenError::Refused(ref why)) if why == "no editor here"),
        "{refused:?}"
    );
    let junk = connect_answer(b"hello\n");
    assert!(matches!(junk, Err(OpenError::Io(_))), "{junk:?}");
}

#[test]
fn a_password_answer_is_the_password_a_refusal_or_an_error_that_says_what_was_wrong() {
    let hex = puddle_types::fixture::suffix().repeat(4);
    assert_eq!(password_answer(&password_line(&hex)).unwrap(), hex);

    let refused = password_answer(&Answer::Refused("not started".into()).to_line());
    assert!(
        matches!(refused, Err(OpenError::Refused(ref why)) if why == "not started"),
        "{refused:?}"
    );

    let bad = password_answer(b"password not-hex!\n");
    assert!(
        matches!(bad, Err(OpenError::Io(ref why)) if why.contains("not one the agent makes")),
        "{bad:?}"
    );

    let other = password_answer(b"ok\n");
    assert!(
        matches!(other, Err(OpenError::Io(ref why)) if why.contains("not an answer to a password request")),
        "{other:?}"
    );

    let binary = password_answer(&[0xff, 0xfe, b'\n']);
    assert!(
        matches!(binary, Err(OpenError::Io(ref why)) if why.contains("not text")),
        "{binary:?}"
    );
}
