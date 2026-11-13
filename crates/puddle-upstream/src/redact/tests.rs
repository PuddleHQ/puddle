// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn a_url_loses_user_info_query_and_fragment() {
    assert_eq!(
        redact_url("http://alice:s3cret@wpad.corp.test:8080/proxy.pac?token=abc#frag"),
        "http://wpad.corp.test:8080/proxy.pac"
    );
    assert_eq!(redact_url("https://h.test"), "https://h.test");
}

#[test]
fn text_with_a_url_or_token_is_cleaned() {
    let out = redact_text(
        "failed at http://bob:pw123@proxy.test:3128/ with Negotiate YIIBCgYGKwYBBQUCoIH5MIH2",
    );
    assert!(!out.contains("pw123") && !out.contains("bob") && !out.contains("YIIB"));
    assert!(out.contains("proxy.test:3128") && out.contains("Negotiate [redacted]"));
}

#[test]
fn plain_words_after_a_scheme_word_stay() {
    assert_eq!(
        redact_text("the proxy offers Basic, NTLM"),
        "the proxy offers Basic, NTLM"
    );
    assert_eq!(redact_text("Basic realm"), "Basic realm");
}

#[test]
fn control_characters_and_length_are_bounded() {
    assert_eq!(redact_text("a\r\nb\u{0}c"), "a b c");
    let long = "x ".repeat(500);
    assert!(redact_text(&long).chars().count() <= MAX_TEXT + 1);
}
