// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

fn json(text: &str) -> TokenBody {
    TokenBody::parse(Some("application/json"), text.as_bytes()).unwrap()
}

#[test]
fn a_dotted_path_reads_and_replaces_a_nested_string() {
    let mut body = json(r#"{"data":{"token":"real","n":1},"token":"top"}"#);
    assert_eq!(body.text_at("data.token").as_deref(), Some("real"));
    assert_eq!(body.text_at("token").as_deref(), Some("top"));
    assert!(body.set_text_at("data.token", "fake"));
    assert_eq!(
        *body.render(),
        br#"{"data":{"n":1,"token":"fake"},"token":"top"}"#.to_vec()
    );
}

#[test]
fn a_path_that_does_not_end_in_a_string_is_not_found_or_changed() {
    let mut body = json(r#"{"data":{"token":7,"list":["x"]},"s":"v"}"#);
    for path in [
        "data.token",
        "data.list",
        "data.missing",
        "s.x",
        "data..token",
        ".data",
        "",
    ] {
        assert!(body.text_at(path).is_none(), "{path}");
        assert!(!body.set_text_at(path, "z"), "{path}");
    }
    assert_eq!(
        *body.render(),
        br#"{"data":{"list":["x"],"token":7},"s":"v"}"#.to_vec()
    );
}

#[test]
fn a_form_has_no_nesting_and_the_path_is_the_field_name() {
    let mut body = TokenBody::parse(
        Some("application/x-www-form-urlencoded"),
        b"a=1&data.token=t",
    )
    .unwrap();
    assert_eq!(body.text_at("data.token").as_deref(), Some("t"));
    assert!(body.set_text_at("data.token", "u"));
    assert_eq!(*body.render(), b"a=1&data.token=u".to_vec());
}
