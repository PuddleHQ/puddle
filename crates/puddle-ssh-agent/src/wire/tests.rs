// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use std::io::{self, Cursor};

/// A stream that reads from a canned answer and records what is written.
struct Canned {
    answer: Cursor<Vec<u8>>,
    written: Vec<u8>,
}

impl Read for Canned {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.answer.read(buf)
    }
}

impl Write for Canned {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.written.extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn string(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&u32::try_from(bytes.len()).unwrap().to_be_bytes());
    out.extend_from_slice(bytes);
}

fn framed(message: &[u8]) -> Vec<u8> {
    let mut out = u32::try_from(message.len()).unwrap().to_be_bytes().to_vec();
    out.extend_from_slice(message);
    out
}

fn answer(keys: &[(&[u8], &str)]) -> Vec<u8> {
    let mut m = vec![MSG_IDENTITIES_ANSWER];
    m.extend_from_slice(&u32::try_from(keys.len()).unwrap().to_be_bytes());
    for (blob, comment) in keys {
        string(&mut m, blob);
        string(&mut m, comment.as_bytes());
    }
    framed(&m)
}

fn run(answer: Vec<u8>) -> (Result<Vec<Identity>, AgentError>, Vec<u8>) {
    let mut s = Canned {
        answer: Cursor::new(answer),
        written: Vec::new(),
    };
    let result = exchange(&mut s);
    (result, s.written)
}

#[test]
fn it_asks_with_the_framed_request_and_reads_the_keys() {
    let mut key = Vec::new();
    string(&mut key, b"ssh-ed25519");
    let (result, written) = run(answer(&[(&key, "one"), (&key, "")]));
    assert_eq!(written, [0, 0, 0, 1, 11]);
    let keys = result.unwrap();
    assert_eq!(keys.len(), 2);
    assert_eq!(keys[0].key_type(), Some("ssh-ed25519"));
    assert_eq!(keys[0].comment(), "one");
    assert_eq!(keys[1].comment(), "");
}

#[test]
fn an_agent_with_no_keys_answers_an_empty_list() {
    assert_eq!(run(answer(&[])).0.unwrap().len(), 0);
}

#[test]
fn a_failure_answer_is_a_refusal() {
    assert!(matches!(
        run(framed(&[MSG_FAILURE])).0,
        Err(AgentError::Refused)
    ));
}

#[test]
fn answers_that_are_not_agent_protocol_are_refused_by_name() {
    let cases: [(Vec<u8>, &str); 7] = [
        (framed(&[]), "an empty message"),
        (framed(&[99]), "not an identities answer"),
        (framed(&[MSG_IDENTITIES_ANSWER]), "a number is cut short"),
        (
            framed(&[MSG_IDENTITIES_ANSWER, 0, 0, 0, 1]),
            "a number is cut short",
        ),
        (
            framed(&[MSG_IDENTITIES_ANSWER, 0, 0, 0, 1, 0, 0, 0, 9, 1]),
            "a string is longer than its message",
        ),
        (
            framed(&[MSG_IDENTITIES_ANSWER, 0, 0, 0, 0, 7]),
            "the answer has bytes after its keys",
        ),
        (
            u32::try_from(MAX_MESSAGE + 1)
                .unwrap()
                .to_be_bytes()
                .to_vec(),
            "a message over 256 KiB",
        ),
    ];
    for (bytes, why) in cases {
        let answer = run(bytes).0;
        assert!(
            matches!(&answer, Err(AgentError::Protocol(got)) if *got == why),
            "{why}: {answer:?}"
        );
    }
}

#[test]
fn a_message_of_exactly_the_limit_is_read() {
    // The literal, so a changed `MAX_MESSAGE` cannot move the test with it.
    assert_eq!(MAX_MESSAGE, 262_144);
    let mut m = vec![MSG_FAILURE];
    m.resize(262_144, 0);
    assert!(matches!(run(framed(&m)).0, Err(AgentError::Refused)));
}

#[test]
fn a_hang_up_is_an_io_error() {
    for bytes in [vec![], vec![0, 0], vec![0, 0, 0, 5, 12]] {
        let answer = run(bytes).0;
        assert!(
            matches!(&answer, Err(AgentError::Io(e)) if e.kind() == io::ErrorKind::UnexpectedEof),
            "{answer:?}"
        );
    }
}

#[test]
fn a_huge_count_is_cut_at_the_limit_without_allocating_for_it() {
    let mut body = u32::MAX.to_be_bytes().to_vec();
    for _ in 0..MAX_IDENTITIES {
        string(&mut body, b"k");
        string(&mut body, b"");
    }
    body.extend_from_slice(b"trailing bytes are ignored once cut");
    assert_eq!(parse_identities(&body).unwrap().len(), MAX_IDENTITIES);
    // A count over what the bytes hold is cut short, not trusted.
    let short = u32::MAX.to_be_bytes();
    assert!(parse_identities(&short).is_err());
}
