// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn names_a_tool_uses_are_accepted() {
    for good in ["appid", "access_token", "key", "sig", "a[b]", "x.y-z~"] {
        assert_eq!(QueryName::new(good).unwrap().as_str(), good);
    }
    assert!(QueryName::new(&"n".repeat(MAX_QUERY_NAME_LEN)).is_ok());
}

#[test]
fn display_is_exactly_the_name() {
    for name in ["appid", "a.b.c", "a[b]", "x.y-z~", "k", "0", "A_b"] {
        assert_eq!(QueryName::new(name).unwrap().to_string(), name);
    }
    let longest = "n".repeat(MAX_QUERY_NAME_LEN);
    assert_eq!(format!("{}", QueryName::new(&longest).unwrap()), longest);
}

#[test]
fn a_name_that_would_split_or_hide_in_a_query_is_refused() {
    let long = "n".repeat(MAX_QUERY_NAME_LEN + 1);
    for bad in [
        "",
        "a&b",
        "a=b",
        "a#",
        "a%41",
        "a+b",
        "a b",
        "\u{e9}",
        "tab\tx",
        "a\r\nb",
        long.as_str(),
    ] {
        let err = QueryName::new(bad).unwrap_err();
        assert_eq!(err.what(), "query parameter name", "{bad:?}");
    }
}

#[test]
fn deserialising_refuses_what_the_constructor_refuses() {
    let ok: QueryName = serde_json::from_str("\"appid\"").unwrap();
    assert_eq!(ok.as_str(), "appid");
    assert!(serde_json::from_str::<QueryName>("\"a&b\"").is_err());
    assert_eq!(serde_json::to_string(&ok).unwrap(), "\"appid\"");
}
