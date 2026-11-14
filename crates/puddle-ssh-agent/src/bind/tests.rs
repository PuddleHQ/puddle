// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use crate::testing::{TestKey, bind as bind_message, string};

fn body(host: &TestKey, session: &[u8], signature: &[u8], forwarding: bool) -> Vec<u8> {
    let message = bind_message(&host.blob(), session, signature, forwarding);
    // Past the type byte and the extension's name.
    message[1 + 4 + 24..].to_vec()
}

#[test]
fn a_bind_is_read_and_its_signature_checks_over_the_session() {
    let host = TestKey::ed25519(7);
    let session = [3_u8; 32];
    let bind = SessionBind::parse(&body(&host, &session, &host.sign(&session), false)).unwrap();
    assert_eq!(bind.host_key, host.key_data());
    assert_eq!(bind.session_id, session);
    assert!(!bind.forwarding);
    assert!(bind.is_signed());
    let forwarded = SessionBind::parse(&body(&host, &session, &host.sign(&session), true)).unwrap();
    assert!(forwarded.forwarding);
}

#[test]
fn a_signature_over_another_session_or_by_another_key_does_not_check() {
    let host = TestKey::ed25519(7);
    let session = [3_u8; 32];
    let other = SessionBind::parse(&body(&host, &session, &host.sign(&[4; 32]), false)).unwrap();
    assert!(!other.is_signed());
    let stranger = TestKey::ed25519(8);
    let by_stranger =
        SessionBind::parse(&body(&host, &session, &stranger.sign(&session), false)).unwrap();
    assert!(!by_stranger.is_signed());
    let garbage = SessionBind::parse(&body(&host, &session, b"not a signature", false)).unwrap();
    assert!(!garbage.is_signed());
}

#[test]
fn a_bind_with_a_bad_field_is_not_read() {
    let host = TestKey::ed25519(7);
    let sig = host.sign(&[3; 32]);
    let cases: Vec<(Vec<u8>, &str)> = vec![
        (
            [
                string(b"not a key"),
                string(&[3; 32]),
                string(&sig),
                vec![0],
            ]
            .concat(),
            "the bind's host key is not a key",
        ),
        (
            [string(&host.blob()), string(&[]), string(&sig), vec![0]].concat(),
            "the bind's session id has a bad length",
        ),
        (
            [
                string(&host.blob()),
                string(&[3; 129]),
                string(&sig),
                vec![0],
            ]
            .concat(),
            "the bind's session id has a bad length",
        ),
        (
            [
                string(&host.blob()),
                string(&[3; 32]),
                string(&sig),
                vec![2],
            ]
            .concat(),
            "the bind does not end with one flag",
        ),
        (
            [string(&host.blob()), string(&[3; 32]), string(&sig)].concat(),
            "the bind does not end with one flag",
        ),
        (
            [
                string(&host.blob()),
                string(&[3; 32]),
                string(&sig),
                vec![0, 0],
            ]
            .concat(),
            "the bind does not end with one flag",
        ),
        (vec![0, 0], "a number is cut short"),
        (
            [string(&host.blob()), vec![0]].concat(),
            "a number is cut short",
        ),
        (
            [string(&host.blob()), string(&[3; 32]), vec![0, 0, 0, 9]].concat(),
            "a string is longer than its message",
        ),
    ];
    for (body, why) in cases {
        let got = SessionBind::parse(&body);
        assert!(
            matches!(&got, Err(AgentError::Protocol(w)) if *w == why),
            "{why}: {got:?}"
        );
    }
    let longest = [
        string(&host.blob()),
        string(&[3; 128]),
        string(&sig),
        vec![0],
    ]
    .concat();
    assert_eq!(SessionBind::parse(&longest).unwrap().session_id.len(), 128);
    let shortest = [string(&host.blob()), string(&[3; 1]), string(&sig), vec![1]].concat();
    assert_eq!(SessionBind::parse(&shortest).unwrap().session_id.len(), 1);
}
