// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn value_is_true_only_for_the_exact_value() {
    let header = InjectedHeader::new("x-api-key", SecretValue::new("sk-real")).unwrap();
    assert!(header.value_is("sk-real"));
    assert!(!header.value_is("sk-real "));
    assert!(!header.value_is("sk-other"));
    assert!(!header.value_is(""));
}

#[test]
fn a_secret_value_prints_as_redacted() {
    let value = SecretValue::new("sk-real");
    assert_eq!(format!("{value:?}"), "<redacted>");
    let header = InjectedHeader::new("x-api-key", value).unwrap();
    assert!(!format!("{header:?}").contains("sk-real"));
}

#[test]
fn a_refusal_keeps_its_status_code_and_message() {
    let refusal = InjectRefusal::new(403, "not-listed", "that repository is not on the list");
    assert_eq!(refusal.status(), 403);
    assert_eq!(refusal.code(), "not-listed");
    assert_eq!(refusal.message(), "that repository is not on the list");
}

#[test]
fn a_status_a_guest_tool_would_prompt_on_or_outside_4xx_5xx_becomes_502() {
    for status in [200, 399, 401, 407, 600, 0] {
        assert_eq!(
            InjectRefusal::new(status, "c", "m").status(),
            502,
            "{status}"
        );
    }
    for status in [400, 403, 404, 500, 599] {
        assert_eq!(InjectRefusal::new(status, "c", "m").status(), status);
    }
}
