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

/// Managing a checkout whose address carries a user name and password is refused without
/// quoting either: the message shows the address with them masked.
#[test]
fn an_existing_checkouts_address_with_credentials_is_refused_without_echoing_them() {
    let request = AddRepositoryRequest::Existing {
        directory: "api".to_owned(),
        url: "https://bob:not-a-real-secret@github.com/acme/api".to_owned(),
        identity_id: None,
        pull: None,
        push: None,
    };
    let err = request.into_add(|_| true).unwrap_err();
    assert_eq!(
        err.message(),
        "url \"https://***@github.com/acme/api\": remove the user name and password from the URL; Puddle supplies credentials itself"
    );
}
