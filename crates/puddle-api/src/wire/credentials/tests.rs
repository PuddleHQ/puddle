// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;
use puddle_secrets::Tool;

#[test]
fn a_pasted_token_never_shows_in_debug_output() {
    let request: StoreTokenRequest =
        serde_json::from_str(r#"{"host":"github.com","token":"CANARY-9"}"#).unwrap();
    assert!(!format!("{request:?}").contains("CANARY-9"));
    assert!(format!("{request:?}").contains("<redacted>"));
    assert!(
        serde_json::from_str::<StoreTokenRequest>(r#"{"host":"h","token":"t","extra":1}"#).is_err()
    );
}

#[test]
fn what_cannot_be_used_is_refused_in_words() {
    use super::super::identities::{IdentityAuthor, IdentityRequest};
    let bad = CredentialSource::Gh {
        host: "not a host".into(),
        account: "me".into(),
    };
    assert_eq!(bad.into_spec().unwrap_err(), "not a valid host");
    let nameless = IdentityRequest {
        label: "x".into(),
        author: IdentityAuthor {
            name: String::new(),
            email: String::new(),
        },
        credentials: Vec::new(),
    };
    assert!(nameless.into_store_draft().is_err());
}

#[test]
fn problems_are_worded_for_the_user() {
    let text = |e: SourceError| CheckResult::from_error(&e);
    let not_signed_in = text(SourceError::NotSignedIn);
    assert!(not_signed_in.needs_sign_in);
    assert_eq!(
        not_signed_in.problem.as_deref(),
        Some("not signed in, or the sign-in has expired")
    );
    let slow = text(SourceError::Timeout(Tool::Git));
    assert!(slow.needs_sign_in);
    assert!(slow.problem.unwrap().contains("git did not answer in time"));
    let missing = text(SourceError::ToolMissing(Tool::Gh));
    assert!(!missing.needs_sign_in);
    assert_eq!(
        missing.problem.as_deref(),
        Some("gh is not installed or not on PATH")
    );
    assert!(CheckResult::readable().readable);
}
