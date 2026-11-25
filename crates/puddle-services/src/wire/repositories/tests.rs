// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn every_note_has_its_wire_name() {
    for (note, wire) in [
        (
            domain::RemoteNote::CredentialsRemoved,
            "credentials_removed",
        ),
        (domain::RemoteNote::Ssh, "ssh"),
        (domain::RemoteNote::NotHttps, "not_https"),
        (domain::RemoteNote::NoRemote, "no_remote"),
    ] {
        assert_eq!(serde_json::to_value(RemoteNote::from(note)).unwrap(), wire);
    }
}

/// Adding a checkout that is already on the volume quotes its address in a refusal with the
/// user name and password masked, as a clone's refusal does.
#[test]
fn an_existing_checkout_refusal_never_echoes_a_credential() {
    let secret = format!("ghp_{}", "maskedinthemessage");
    let request = AddRepositoryRequest::Existing {
        directory: "api".to_owned(),
        url: format!("https://bob:{secret}@github.com/acme/api"),
        identity_id: None,
        pull: None,
        push: None,
    };
    let refused = request.into_add(|_| true).unwrap_err();
    let message = refused.message();
    assert!(!message.contains(&secret), "{message}");
    assert!(!message.contains("bob"), "{message}");
    assert!(
        message.contains("https://***@github.com/acme/api"),
        "{message}"
    );
}
