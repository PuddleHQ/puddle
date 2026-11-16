// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

fn rejected() -> Unauthorized {
    Unauthorized::noting(|| {})
}

#[test]
fn a_value_that_cannot_be_a_header_is_refused_as_an_unavailable_credential() {
    let refused = credentialed(
        SecretValue::new("Basic a\nb"),
        "identity:1:github.com".to_owned(),
        "Work",
        "github.com/acme",
        rejected(),
    );
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
    let decision = credentialed(
        SecretValue::new("Basic YTpi"),
        "identity:1:github.com".to_owned(),
        "Work",
        "github.com/acme",
        rejected(),
    );
    let InjectDecision::Inject(injection) = decision else {
        panic!("{decision:?}");
    };
    assert_eq!(injection.binding_id(), "identity:1:github.com");
}
