// SPDX-License-Identifier: AGPL-3.0-or-later
use proptest::prelude::*;

use super::*;

fn json(text: &str) -> TokenBody {
    TokenBody::parse(Some("application/json; charset=utf-8"), text.as_bytes()).unwrap()
}

#[test]
fn the_fields_of_a_body_are_listed_with_kinds_and_paths_and_never_a_value() {
    let body = json(r#"{"a":"abc","b":"","c":null,"d":7,"e":{"f":"xy"},"g":[{"h":"z"}]}"#);
    let listed: Vec<(String, String, FieldKind)> = body
        .fields()
        .into_iter()
        .map(|f| (f.name, f.path, f.kind))
        .collect();
    let row = |name: &str, path: &str, kind| (name.to_owned(), path.to_owned(), kind);
    assert_eq!(
        listed,
        [
            row("a", "a", FieldKind::Text(3)),
            row("b", "b", FieldKind::Empty),
            row("c", "c", FieldKind::Empty),
            row("d", "d", FieldKind::Other),
            row("e", "e", FieldKind::Structured),
            row("f", "e.f", FieldKind::Text(2)),
            row("g", "g", FieldKind::Structured),
            row("g", "g", FieldKind::Structured),
            row("h", "g.h", FieldKind::Text(1)),
        ]
    );
    let repeated = form("a=1&a=2&b=");
    let names: Vec<(String, FieldKind)> = repeated
        .fields()
        .into_iter()
        .map(|f| (f.name, f.kind))
        .collect();
    assert_eq!(
        names,
        [
            ("a".to_owned(), FieldKind::Text(1)),
            ("a".to_owned(), FieldKind::Text(1)),
            ("b".to_owned(), FieldKind::Empty),
        ]
    );
}

fn form(text: &str) -> TokenBody {
    TokenBody::parse(Some("Application/X-WWW-Form-Urlencoded"), text.as_bytes()).unwrap()
}

#[test]
fn json_fields_are_read_and_replaced_and_the_rest_is_kept() {
    let mut body = json(
        r#"{"grant_type":"refresh_token","refresh_token":"old","n":1,"nested":{"refresh_token":"deep"}}"#,
    );
    assert_eq!(body.format(), BodyFormat::Json);
    assert_eq!(body.text("refresh_token").as_deref(), Some("old"));
    assert_eq!(body.text("nested"), None, "only text fields are read");
    assert_eq!(body.text("n"), None);
    assert_eq!(body.text("missing"), None);
    assert!(body.set_text("refresh_token", "new"));
    assert!(!body.set_text("missing", "x"));
    let rendered: Value = serde_json::from_slice(&body.render()).unwrap();
    assert_eq!(rendered["refresh_token"], "new");
    assert_eq!(rendered["nested"]["refresh_token"], "deep");
    assert_eq!(rendered["n"], 1);
}

#[test]
fn a_form_keeps_the_exact_bytes_of_the_fields_it_does_not_touch() {
    let mut body =
        form("client_id=abc&grant_type=refresh_token&refresh_token=old%2Bvalue&scope=repo+gist");
    assert_eq!(body.format(), BodyFormat::Form);
    assert_eq!(body.text("refresh_token").as_deref(), Some("old+value"));
    assert_eq!(body.text("scope").as_deref(), Some("repo gist"));
    assert!(body.set_text("refresh_token", "gho_a+b/c"));
    assert_eq!(
        *body.render(),
        b"client_id=abc&grant_type=refresh_token&refresh_token=gho_a%2Bb%2Fc&scope=repo+gist"
    );
}

#[test]
fn a_form_field_that_appears_twice_is_not_read() {
    let mut body = form("refresh_token=one&refresh_token=two&x=1");
    assert_eq!(body.text("refresh_token"), None);
    assert!(!body.set_text("refresh_token", "new"));
    assert_eq!(*body.render(), b"refresh_token=one&refresh_token=two&x=1");
    assert_eq!(body.text("x").as_deref(), Some("1"));
}

#[test]
fn a_form_with_empty_parts_and_a_field_without_a_value_still_parses() {
    let mut body = form("&a&b=&&c=1&");
    assert_eq!(body.text("a").as_deref(), Some(""));
    assert_eq!(body.text("b").as_deref(), Some(""));
    assert!(body.set_text("c", "2"));
    assert_eq!(*body.render(), b"a&b=&c=2");
    assert_eq!(*form("").render(), b"");
}

#[test]
fn reads_says_which_content_types_parse() {
    for yes in [
        "application/json",
        "Application/JSON; charset=utf-8",
        "application/x-www-form-urlencoded",
        "application/problem+json",
    ] {
        assert!(TokenBody::reads(Some(yes)), "{yes}");
    }
    for no in ["text/html", "application/xml", "", "application/jsonp"] {
        assert!(!TokenBody::reads(Some(no)), "{no}");
    }
    assert!(!TokenBody::reads(None));
}

#[test]
fn the_content_type_decides_and_other_types_are_refused() {
    assert_eq!(
        TokenBody::parse(Some("text/plain"), b"{}").unwrap_err(),
        TokenBodyError::Format
    );
    assert_eq!(
        TokenBody::parse(None, b"{}").unwrap_err(),
        TokenBodyError::Format
    );
    assert!(TokenBody::parse(Some("application/vnd.x+json"), b"{}").is_ok());
}

#[test]
fn bodies_that_do_not_parse_are_refused() {
    for (content_type, body) in [
        ("application/json", &b"not json"[..]),
        ("application/json", b"[1,2]"),
        ("application/json", b"\"text\""),
        ("application/json", b""),
        ("application/x-www-form-urlencoded", b"\xff\xfe=1"),
    ] {
        assert_eq!(
            TokenBody::parse(Some(content_type), body).unwrap_err(),
            TokenBodyError::Malformed,
            "{content_type} {body:?}"
        );
    }
}

#[test]
fn hostile_json_does_not_panic() {
    let deep = format!("{}1{}", "[".repeat(10_000), "]".repeat(10_000));
    let object = format!("{{\"a\":{deep}}}");
    assert_eq!(
        TokenBody::parse(Some("application/json"), object.as_bytes()).unwrap_err(),
        TokenBodyError::Malformed
    );
}

proptest! {
    #[test]
    fn nothing_a_guest_sends_panics_the_parser(
        bytes in prop::collection::vec(any::<u8>(), 0..400),
        json in any::<bool>(),
    ) {
        let content_type = if json { "application/json" } else { "application/x-www-form-urlencoded" };
        if let Ok(mut body) = TokenBody::parse(Some(content_type), &bytes) {
            let _ = body.text("refresh_token");
            let _ = body.set_text("refresh_token", "x");
            let _ = body.render();
        }
    }

    #[test]
    fn replacing_a_json_field_changes_that_field_only(
        fields in prop::collection::btree_map("[a-z_]{1,12}", "[ -~]{0,30}", 1..8),
        pick in 0_usize..8,
        value in "[ -~]{0,30}",
    ) {
        let object: Map<String, Value> = fields
            .iter()
            .map(|(k, v)| (k.clone(), Value::String(v.clone())))
            .collect();
        let text = Value::Object(object).to_string();
        let mut body = TokenBody::parse(Some("application/json"), text.as_bytes()).unwrap();
        let names: Vec<&String> = fields.keys().collect();
        let name = names[pick % names.len()].clone();
        prop_assert!(body.set_text(&name, &value));
        let after: Value = serde_json::from_slice(&body.render()).unwrap();
        for (key, old) in &fields {
            let want = if *key == name { &value } else { old };
            prop_assert_eq!(after[key].as_str(), Some(want.as_str()));
        }
    }

    #[test]
    fn replacing_a_form_field_keeps_every_other_pair_byte_for_byte(
        others in prop::collection::vec("[a-z]{1,6}=[A-Za-z0-9%+._~-]{0,12}", 0..6),
        before in 0_usize..6,
        value in "[ -~]{0,30}",
    ) {
        let at = before.min(others.len());
        let mut parts = others.clone();
        parts.insert(at, "zz_target=old".to_owned());
        // A pair of ours must stay unique for the field to be read.
        prop_assume!(!others.iter().any(|p| p.starts_with("zz_target=")));
        let sent = parts.join("&");
        let mut body = TokenBody::parse(Some("application/x-www-form-urlencoded"), sent.as_bytes()).unwrap();
        prop_assert!(body.set_text("zz_target", &value));
        let rendered = String::from_utf8(body.render().to_vec()).unwrap();
        let pieces: Vec<&str> = rendered.split('&').collect();
        prop_assert_eq!(pieces.len(), parts.len());
        for (i, (got, was)) in pieces.iter().zip(&parts).enumerate() {
            if i != at {
                prop_assert_eq!(got, was);
            }
        }
        let again = TokenBody::parse(Some("application/x-www-form-urlencoded"), rendered.as_bytes()).unwrap();
        let read = again.text("zz_target");
        prop_assert_eq!(read.as_deref(), Some(value.as_str()));
    }
}
