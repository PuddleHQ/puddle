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
