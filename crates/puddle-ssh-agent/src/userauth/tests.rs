// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use crate::testing::{login as login_data, string};

#[test]
fn a_publickey_login_is_read_with_its_session_and_key() {
    let data = login_data(b"SESSION", b"KEY", None);
    assert_eq!(
        read(&data),
        Signed::Login {
            session_id: b"SESSION",
            key: b"KEY",
            host_key: None
        }
    );
}

#[test]
fn a_host_bound_login_names_the_server_key_too() {
    let data = login_data(b"SESSION", b"KEY", Some(b"HOST"));
    assert_eq!(
        read(&data),
        Signed::Login {
            session_id: b"SESSION",
            key: b"KEY",
            host_key: Some(b"HOST")
        }
    );
}

#[test]
fn an_sshsig_request_is_told_apart() {
    let mut data = b"SSHSIG".to_vec();
    data.extend(string(b"git"));
    assert_eq!(read(&data), Signed::SshSig);
}

/// The login with one field replaced: (offset in the field list, the new bytes).
fn changed(field: usize, bytes: &[u8]) -> Vec<u8> {
    let mut fields: Vec<Vec<u8>> = vec![
        string(b"SESSION"),
        vec![50],
        string(b"git"),
        string(b"ssh-connection"),
        string(b"publickey"),
        vec![1],
        string(b"ssh-ed25519"),
        string(b"KEY"),
    ];
    fields[field] = bytes.to_vec();
    fields.concat()
}

#[test]
fn anything_but_a_whole_login_is_other() {
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("not a userauth message", changed(1, &[51])),
        ("another service", changed(3, &string(b"ssh-userauth"))),
        ("another method", changed(4, &string(b"password"))),
        ("a query without a signature", changed(5, &[0])),
        ("a flag that is not a bool", changed(5, &[2])),
        (
            "bytes left over",
            [changed(7, &string(b"KEY")), vec![0]].concat(),
        ),
        ("cut short", changed(7, &[0, 0, 0, 9])),
        ("cut before the type", string(b"SESSION")),
        ("cut before the flag", changed(5, &[])[..50].to_vec()),
        ("no session at all", vec![0, 0]),
    ];
    for (why, data) in cases {
        assert_eq!(read(&data), Signed::Other, "{why}");
    }
    // The host-bound method without its host key is cut short.
    let hostbound = changed(4, &string(b"publickey-hostbound-v00@openssh.com"));
    assert_eq!(read(&hostbound), Signed::Other);
    // A user name that is not there.
    assert_eq!(read(&changed(2, &[0, 0, 0, 200])), Signed::Other);
    // A method that is not there.
    assert_eq!(
        read(
            &[
                string(b"S"),
                vec![50],
                string(b"u"),
                string(b"ssh-connection")
            ]
            .concat()
        ),
        Signed::Other
    );
    // An algorithm name that is not there.
    assert_eq!(read(&changed(6, &[0, 0, 0, 200])), Signed::Other);
}

#[test]
fn the_unchanged_helper_login_is_a_login() {
    assert!(matches!(
        read(&changed(7, &string(b"KEY"))),
        Signed::Login { .. }
    ));
}
