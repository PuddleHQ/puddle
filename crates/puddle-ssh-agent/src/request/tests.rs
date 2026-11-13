// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use crate::testing::{framed as frame, sign_request as sign_message, string};

#[test]
fn each_request_type_is_read_with_its_fields() {
    assert_eq!(parse(&[11]).unwrap(), Request::List);
    assert_eq!(
        parse(&sign_message(b"KEY", b"DATA", 4)).unwrap(),
        Request::Sign {
            key: b"KEY".to_vec(),
            data: b"DATA".to_vec(),
            flags: 4
        }
    );
    let mut ext = vec![27];
    ext.extend(string(b"query"));
    ext.extend_from_slice(b"rest");
    assert_eq!(
        parse(&ext).unwrap(),
        Request::Extension {
            name: b"query".to_vec(),
            body: b"rest".to_vec()
        }
    );
    for other in [17, 18, 19, 22, 23, 25, 1, 200] {
        assert_eq!(parse(&[other, 9, 9]).unwrap(), Request::Other(other));
    }
}

#[test]
fn a_request_cut_short_or_with_bytes_left_over_is_not_read() {
    let cases: [(Vec<u8>, &str); 5] = [
        (vec![], "an empty message"),
        (vec![11, 0], "the request has bytes after its fields"),
        (
            vec![13, 0, 0, 0, 9, 1],
            "a string is longer than its message",
        ),
        (
            sign_message(b"K", b"D", 0)[..12].to_vec(),
            "a number is cut short",
        ),
        (
            [sign_message(b"K", b"D", 0), vec![0]].concat(),
            "the request has bytes after its fields",
        ),
    ];
    for (message, why) in cases {
        let got = parse(&message);
        assert!(
            matches!(&got, Err(AgentError::Protocol(w)) if *w == why),
            "{message:?}: {got:?}"
        );
    }
}

#[test]
fn the_answers_are_framed_agent_messages() {
    assert_eq!(failure(), frame(&[5]));
    assert_eq!(success(), frame(&[6]));
    assert_eq!(
        signature(b"SIG"),
        frame(&[vec![14], string(b"SIG")].concat())
    );
    assert_eq!(
        sign_request(b"K", b"D", 2),
        frame(&sign_message(b"K", b"D", 2))
    );
}

#[test]
fn a_key_list_answer_carries_the_keys_without_their_comments() {
    let a = Identity::new(b"A".to_vec(), "me@desk".into());
    let b = Identity::new(b"BB".to_vec(), "/home/me/.ssh/id".into());
    let want = [
        vec![12, 0, 0, 0, 2],
        string(b"A"),
        string(b""),
        string(b"BB"),
        string(b""),
    ]
    .concat();
    assert_eq!(identities(&[&a, &b]), frame(&want));
    assert_eq!(identities(&[]), frame(&[12, 0, 0, 0, 0]));
}

#[test]
fn a_sign_answer_from_the_host_agent_gives_its_signature_or_says_why_not() {
    let ok = [vec![14], string(b"SIG")].concat();
    assert_eq!(parse_signature(&ok).unwrap(), b"SIG");
    assert!(matches!(parse_signature(&[5]), Err(AgentError::Refused)));
    let cases: [(Vec<u8>, &str); 4] = [
        (vec![12, 0, 0, 0, 0], "not a sign response"),
        (vec![], "not a sign response"),
        (vec![14, 0, 0], "a number is cut short"),
        (
            [ok.clone(), vec![1]].concat(),
            "the answer has bytes after its signature",
        ),
    ];
    for (message, why) in cases {
        let got = parse_signature(&message);
        assert!(
            matches!(&got, Err(AgentError::Protocol(w)) if *w == why),
            "{message:?}: {got:?}"
        );
    }
}
