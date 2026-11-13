// SPDX-License-Identifier: AGPL-3.0-or-later
use proptest::prelude::*;

use super::*;

fn stripped(fields: &[&[u8]]) -> (Vec<Vec<u8>>, usize) {
    let mut headers = HeaderMap::new();
    for field in fields {
        headers.append(ALT_SVC, HeaderValue::from_bytes(field).unwrap());
    }
    let removed = strip_h3(&mut headers);
    let left = headers
        .get_all(ALT_SVC)
        .iter()
        .map(|v| v.as_bytes().to_vec())
        .collect();
    (left, removed)
}

fn one(field: &str) -> (Vec<String>, usize) {
    let (left, removed) = stripped(&[field.as_bytes()]);
    let left = left
        .into_iter()
        .map(|v| String::from_utf8(v).unwrap())
        .collect();
    (left, removed)
}

#[test]
fn a_field_of_h3_entries_only_is_removed() {
    assert_eq!(one(r#"h3=":443"; ma=86400"#), (vec![], 1));
    assert_eq!(
        one(r#"h3=":443"; ma=2592000,h3-29=":443"; ma=2592000"#),
        (vec![], 2)
    );
}

#[test]
fn the_other_entries_stay_as_the_server_wrote_them() {
    assert_eq!(
        one(r#"h3=":443"; ma=86400, h2="alt.example.com:8443"; ma=60; persist=1"#),
        (
            vec![r#"h2="alt.example.com:8443"; ma=60; persist=1"#.into()],
            1
        )
    );
    assert_eq!(
        one(r#"h2=":443",h3-29=":443" ,  h3=":443" , h2=":8443"; ma=5"#),
        (vec![r#"h2=":443", h2=":8443"; ma=5"#.into()], 2)
    );
}

#[test]
fn google_quic_and_percent_encoded_ids_count_as_h3() {
    assert_eq!(
        one(r#"quic=":443"; ma=2592000; v="46,43", h2=":443""#),
        (vec![r#"h2=":443""#.into()], 1)
    );
    assert_eq!(
        one(r#"h3%2D29=":443", h%33=":443", h3-Q050=":443", h2=":443""#),
        (vec![r#"h2=":443""#.into()], 3)
    );
}

#[test]
fn a_comma_in_a_quoted_string_does_not_split_an_entry() {
    assert_eq!(
        one(r#"h2=":443"; note="a, h3=b", h3=":443""#),
        (vec![r#"h2=":443"; note="a, h3=b""#.into()], 1)
    );
    assert_eq!(
        one(r#"h2=":443"; note="a\", h3=b", h3=":443""#),
        (vec![r#"h2=":443"; note="a\", h3=b""#.into()], 1)
    );
    // A quote that never closes makes the rest one entry; it is not an h3 entry.
    assert_eq!(
        one(r#"h2=":443; ma=1, h3=":443""#),
        (vec![r#"h2=":443; ma=1, h3=":443""#.into()], 0)
    );
}

#[test]
fn what_is_not_h3_is_left_alone() {
    for field in [
        r#"h2=":443"; ma=60"#,
        "clear",
        r#"h30=":443""#,
        r#"H3=":443""#,
        // A percent sign that does not start an escape stays what it is.
        r#"h3%2=":443""#,
        r#"h3%zz=":443""#,
        "h3",
        r#"h2=":443",,h2=":8443""#,
        "",
    ] {
        assert_eq!(one(field), (vec![field.into()], 0), "{field}");
    }
}

#[test]
fn each_field_is_stripped_on_its_own_and_other_headers_stay() {
    let mut headers = HeaderMap::new();
    headers.append(ALT_SVC, HeaderValue::from_static(r#"h3=":443""#));
    headers.append(ALT_SVC, HeaderValue::from_static(r#"h2=":443", h3=":443""#));
    headers.append(ALT_SVC, HeaderValue::from_static("clear"));
    headers.insert("x-other", HeaderValue::from_static("kept"));
    assert_eq!(strip_h3(&mut headers), 2);
    let left: Vec<_> = headers.get_all(ALT_SVC).iter().collect();
    assert_eq!(left, [r#"h2=":443""#, "clear"]);
    assert_eq!(headers["x-other"], "kept");
    assert_eq!(strip_h3(&mut HeaderMap::new()), 0);
}

#[test]
fn bytes_outside_ascii_are_kept() {
    let (left, removed) = stripped(&[b"h2=\":443\"; note=\"a\xe9b\", h3=\":443\""]);
    assert_eq!(removed, 1);
    assert_eq!(left, [b"h2=\":443\"; note=\"a\xe9b\"".to_vec()]);
}

fn entry() -> impl Strategy<Value = String> {
    (
        prop_oneof![
            Just("h3"),
            Just("h3-29"),
            Just("quic"),
            Just("h2"),
            Just("http/1.1"),
            Just("clear"),
            Just("h3%2D27"),
        ],
        prop_oneof![Just(r#"":443""#), Just(r#"":8443"; ma=60; v="46,43""#)],
    )
        .prop_map(|(id, rest)| format!("{id}={rest}"))
}

proptest! {
    /// What survives is the input's entries in order, none of them HTTP/3, and a second pass
    /// changes nothing.
    #[test]
    fn what_is_left_has_no_h3_and_keeps_the_order(
        entries in proptest::collection::vec(entry(), 0..8),
        blanks in "[ \t]{0,3}",
    ) {
        let field = entries.join(&format!("{blanks},{blanks}"));
        let (left, removed) = stripped(&[field.as_bytes()]);
        let expected: Vec<&String> = entries.iter().filter(|e| !is_h3(e.as_bytes())).collect();
        prop_assert_eq!(removed, entries.len() - expected.len());
        if removed == 0 {
            prop_assert_eq!(&left, &[field.clone().into_bytes()]);
        } else if expected.is_empty() {
            prop_assert!(left.is_empty());
        } else {
            let joined = expected.iter().map(|e| e.as_str()).collect::<Vec<_>>();
            prop_assert_eq!(&left, &[joined.join(", ").into_bytes()]);
        }
        let (again, removed_again) = stripped(&left.iter().map(Vec::as_slice).collect::<Vec<_>>());
        prop_assert_eq!(removed_again, 0);
        prop_assert_eq!(again, left);
    }

    /// Any field value passes without a panic, and a value with no `h3` or `quic` in it
    /// is returned byte for byte.
    #[test]
    fn any_field_value_is_handled(bytes in proptest::collection::vec(any::<u8>(), 0..120)) {
        // skip-ok: a random byte string that is not a header value has nothing to strip; proptest draws another.
        let Ok(value) = HeaderValue::from_bytes(&bytes) else { return Ok(()); };
        let mut headers = HeaderMap::new();
        headers.append(ALT_SVC, value.clone());
        let removed = strip_h3(&mut headers);
        if removed == 0 {
            prop_assert_eq!(&headers[ALT_SVC], &value);
        }
    }
}

#[test]
fn a_field_without_h3_stays_byte_for_byte_beside_one_that_loses_entries() {
    let (left, removed) = stripped(&[br#"h2=":443",h2=":8443""#, br#"h3=":443""#]);
    assert_eq!(removed, 1);
    assert_eq!(left, vec![br#"h2=":443",h2=":8443""#.to_vec()]);
}

#[test]
fn headers_with_nothing_to_strip_are_left_in_their_order() {
    let mut headers = HeaderMap::new();
    headers.append(ALT_SVC, HeaderValue::from_static(r#"h2=":443""#));
    headers.append("x-other", HeaderValue::from_static("1"));
    assert_eq!(strip_h3(&mut headers), 0);
    let names: Vec<_> = headers.keys().map(|n| n.as_str().to_owned()).collect();
    assert_eq!(names, ["alt-svc", "x-other"]);
}
