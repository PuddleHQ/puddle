// SPDX-License-Identifier: GPL-3.0-or-later
//! The agent protocol's framing and the one exchange Puddle needs (`draft-miller-ssh-agent`):
//! `SSH_AGENTC_REQUEST_IDENTITIES` (11) answered by `SSH_AGENT_IDENTITIES_ANSWER` (12), or
//! `SSH_AGENT_FAILURE` (5) from an agent that is locked or refuses.

use std::io::{Read, Write};

use crate::error::AgentError;
use crate::identity::Identity;

const MSG_FAILURE: u8 = 5;
const MSG_REQUEST_IDENTITIES: u8 = 11;
const MSG_IDENTITIES_ANSWER: u8 = 12;

/// The longest answer read: OpenSSH's own limit for an agent message is 256 KiB, and a list of
/// keys fits in a small part of it. A longer length is an agent that is not one.
pub const MAX_MESSAGE: usize = 256 * 1024;

/// The most keys read from one answer; an agent holding more is cut at this and the rest ignored
/// (the doctor counts, the filtered agent never offers more than it can show).
pub const MAX_IDENTITIES: usize = 1024;

/// The request for the key list, framed: length 1, type 11.
#[must_use]
pub fn request_identities() -> [u8; 5] {
    [0, 0, 0, 1, MSG_REQUEST_IDENTITIES]
}

/// Reads one length-prefixed string from the front of `buf` and advances it.
///
/// # Errors
///
/// [`AgentError::Protocol`] when the length is missing or longer than what is left.
pub fn take_string<'a>(buf: &mut &'a [u8]) -> Result<&'a [u8], AgentError> {
    let len = take_u32(buf)? as usize;
    if buf.len() < len {
        return Err(AgentError::Protocol("a string is longer than its message"));
    }
    let (head, tail) = buf.split_at(len);
    *buf = tail;
    Ok(head)
}

fn take_u32(buf: &mut &[u8]) -> Result<u32, AgentError> {
    let Some((head, tail)) = buf.split_first_chunk::<4>() else {
        return Err(AgentError::Protocol("a number is cut short"));
    };
    *buf = tail;
    Ok(u32::from_be_bytes(*head))
}

/// Parses the body of an identities answer (after the type byte).
///
/// # Errors
///
/// [`AgentError::Protocol`] when the count or a key is cut short or has bytes left over.
pub fn parse_identities(mut body: &[u8]) -> Result<Vec<Identity>, AgentError> {
    let count = take_u32(&mut body)? as usize;
    let mut keys = Vec::with_capacity(count.min(MAX_IDENTITIES));
    for _ in 0..count.min(MAX_IDENTITIES) {
        let blob = take_string(&mut body)?.to_vec();
        let comment = String::from_utf8_lossy(take_string(&mut body)?).into_owned();
        keys.push(Identity::new(blob, comment));
    }
    if count <= MAX_IDENTITIES && !body.is_empty() {
        return Err(AgentError::Protocol("the answer has bytes after its keys"));
    }
    Ok(keys)
}

/// Sends the key-list request on `stream` and reads the answer.
///
/// # Errors
///
/// [`AgentError::Io`] when the stream breaks (an agent that closes without answering included),
/// [`AgentError::Refused`] for a failure answer, [`AgentError::Protocol`] for anything else.
pub fn exchange<S: Read + Write>(stream: &mut S) -> Result<Vec<Identity>, AgentError> {
    stream
        .write_all(&request_identities())
        .and_then(|()| stream.flush())
        .map_err(AgentError::Io)?;
    let mut len = [0_u8; 4];
    stream.read_exact(&mut len).map_err(AgentError::Io)?;
    let len = u32::from_be_bytes(len) as usize;
    if len == 0 {
        return Err(AgentError::Protocol("an empty message"));
    }
    if len > MAX_MESSAGE {
        return Err(AgentError::Protocol("a message over 256 KiB"));
    }
    let mut message = vec![0_u8; len];
    stream.read_exact(&mut message).map_err(AgentError::Io)?;
    match message.split_first() {
        Some((&MSG_IDENTITIES_ANSWER, body)) => parse_identities(body),
        Some((&MSG_FAILURE, _)) => Err(AgentError::Refused),
        _ => Err(AgentError::Protocol("not an identities answer")),
    }
}

#[cfg(test)]
mod tests {
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
        let mut m = vec![MSG_FAILURE];
        m.resize(MAX_MESSAGE, 0);
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
}
