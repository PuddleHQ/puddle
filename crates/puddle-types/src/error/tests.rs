// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn message_names_kind_value_and_reason() {
    let e = ValidationError::new("workspace name", "Foo", "must be lower-case");
    assert_eq!(
        e.to_string(),
        r#"invalid workspace name "Foo": must be lower-case"#
    );
    assert_eq!(e.what(), "workspace name");
    assert_eq!(e.reason(), "must be lower-case");
}

#[test]
fn long_values_are_truncated() {
    let long = "x".repeat(10_000);
    let e = ValidationError::new("thing", &long, "too long");
    assert!(e.to_string().len() < 120, "{e}");
    assert!(e.to_string().contains('…'));
}

#[test]
fn control_characters_are_escaped_in_the_message() {
    let e = ValidationError::new("thing", "a\nb\u{1b}[31m", "bad");
    assert!(!e.to_string().contains('\n'));
    assert!(!e.to_string().contains('\u{1b}'));
}
