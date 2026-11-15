// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn a_value_that_cannot_be_a_header_is_refused_as_an_unavailable_credential() {
    let refused = authorization_header(SecretValue::new("Basic a\nb"), "Work", "github.com/acme")
        .expect_err("a newline is not allowed in a header value");
    let InjectDecision::Refuse(refusal) = refused else {
        panic!("{refused:?}");
    };
    assert_eq!(refusal.status(), 502);
    assert_eq!(refusal.code(), ReasonCode::CredentialUnavailable);
    assert!(refusal.message().contains("identity Work"));
    assert!(refusal.message().contains("github.com/acme"));
}

#[test]
fn a_plain_value_becomes_the_authorization_header() {
    let header =
        authorization_header(SecretValue::new("Basic YTpi"), "Work", "github.com/acme").unwrap();
    assert!(header.value_is("Basic YTpi"));
}
