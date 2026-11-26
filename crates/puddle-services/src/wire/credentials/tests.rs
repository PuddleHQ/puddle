// SPDX-License-Identifier: AGPL-3.0-or-later
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
    assert_eq!(bad.into_spec().unwrap_err().message, "not a valid host");
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
    let gh = SourceSpec::Gh {
        host: HostName::new("github.com").unwrap(),
        account: puddle_secrets::AccountName::new("octo").unwrap(),
    };
    let text = |e: SourceError| CheckResult::from_error(&gh, &e);
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

#[test]
fn an_azure_cli_problem_carries_the_step_and_never_asks_for_a_sign_in_button() {
    let az = SourceSpec::AzCli {
        tenant: puddle_secrets::TenantId::new("contoso.onmicrosoft.com").unwrap(),
        org: None,
    };
    let signed_out = CheckResult::from_error(&az, &SourceError::NotSignedIn);
    assert!(!signed_out.needs_sign_in);
    assert!(
        signed_out
            .fix_step
            .unwrap()
            .contains("az login --tenant contoso.onmicrosoft.com")
    );
    let slow = CheckResult::from_error(&az, &SourceError::Timeout(Tool::Az));
    assert!(!slow.needs_sign_in);
    let missing = CheckResult::from_error(&az, &SourceError::ToolMissing(Tool::Az));
    assert!(
        missing
            .fix_step
            .unwrap()
            .starts_with("Install the Azure CLI")
    );
    assert_eq!(CheckResult::readable().fix_step, None);
}
