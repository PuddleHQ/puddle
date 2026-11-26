// SPDX-License-Identifier: AGPL-3.0-or-later
use puddle_secrets::Tool;

use super::*;
use crate::error::ApiError;

#[test]
fn a_sign_in_that_could_not_start_says_what_happened_and_what_to_try() {
    let gh = SourceSpec::Gh {
        host: puddle_secrets::HostName::new("github.com").unwrap(),
        account: puddle_secrets::AccountName::new("octo").unwrap(),
    };
    let said = |err| sign_in_error(&gh, err).to_string();
    assert_eq!(
        said(SignInError::NoPrompt(
            Tool::Gh,
            "x509: unknown authority".into()
        )),
        "gh did not show a sign-in code: x509: unknown authority; try again, or sign in from a terminal"
    );
    assert_eq!(
        said(SignInError::Ended(Tool::Git, String::new())),
        "git ended without signing in; Git Credential Manager is what signs in here, so check that Git can run it"
    );
    assert_eq!(
        said(SignInError::Source(SourceError::ToolMissing(Tool::Gh))),
        "gh is not installed or not on PATH"
    );
    assert!(said(SignInError::NothingToSignIn).starts_with("a pasted token has nothing"));
}

#[test]
fn no_sign_in_starts_for_the_azure_cli_and_the_answer_names_the_terminal_step() {
    let az = SourceSpec::AzCli {
        tenant: puddle_secrets::TenantId::new("contoso.onmicrosoft.com").unwrap(),
        org: None,
    };
    assert_eq!(
        sign_in_error(&az, SignInError::NothingToSignIn).to_string(),
        "Puddle does not sign in to the Azure CLI; run `az login --tenant contoso.onmicrosoft.com` in a terminal"
    );
}

#[tokio::test]
async fn the_fake_starts_no_sign_in_for_the_azure_cli() {
    let fake = FakeCredentials::new();
    let az = SourceSpec::AzCli {
        tenant: puddle_secrets::TenantId::new("contoso.onmicrosoft.com").unwrap(),
        org: None,
    };
    assert!(matches!(
        fake.sign_in(az).await,
        Err(CredentialsError::Invalid(_))
    ));
}

#[tokio::test]
async fn the_fake_keeps_a_pasted_value_under_its_id_until_forgotten() {
    let fake = FakeCredentials::new();
    fake.store_token(
        UserId::OWNER,
        TokenScope {
            host: puddle_secrets::HostName::new("github.com").unwrap(),
            org: None,
        },
        Secret::new("ghp_0123456789abcdef".into()),
    )
    .await
    .unwrap();
    let [kept] = fake.tokens().try_into().unwrap();
    let id = StoredId::new(kept).unwrap();
    assert_eq!(fake.tokens(), [id.as_str()]);
    assert_eq!(
        fake.token_value(id.as_str()).as_deref(),
        Some("ghp_0123456789abcdef")
    );
    fake.forget_token(id.clone()).await.unwrap();
    assert_eq!(fake.token_value(id.as_str()), None);
    fake.forget_all_tokens();
    assert_eq!(fake.tokens(), Vec::<String>::new());
}

#[test]
fn a_build_without_a_credential_store_keeps_nothing_and_says_so_at_every_call() {
    let id = StoredId::new("env-1").unwrap();
    let none = NoSecretStore;
    assert!(none.get(&id).is_err());
    assert!(none.set(&id, &Secret::new("v".into())).is_err());
    assert!(none.delete(&id).is_err());
}

#[test]
fn every_refusal_has_its_status_and_the_services_describe_themselves_without_a_value() {
    use axum::http::StatusCode;
    for (err, status) in [
        (
            CredentialsError::Unavailable("x".into()),
            StatusCode::SERVICE_UNAVAILABLE,
        ),
        (
            CredentialsError::Invalid("x".into()),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            CredentialsError::Internal("x".into()),
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
    ] {
        assert_eq!(ApiError::from(err).status(), status);
    }
    let real = HostCredentials::new(
        ToolPaths::new(None, None),
        Arc::new(puddle_secrets::MemoryStore::new()),
    )
    .with_sign_ins(SignIns::new(ToolPaths::new(None, None)));
    assert_eq!(format!("{real:?}"), "HostCredentials { .. }");
    let nothing = NoCredentials;
    let gh = SourceSpec::Gh {
        host: puddle_secrets::HostName::new("github.com").unwrap(),
        account: puddle_secrets::AccountName::new("me").unwrap(),
    };
    assert_eq!(nothing.last_read(&gh), None);
    assert_eq!(real.last_read(&gh), None);
    assert_eq!(
        format!("{:?}", FakeCredentials::new()),
        "FakeCredentials { .. }"
    );
}
