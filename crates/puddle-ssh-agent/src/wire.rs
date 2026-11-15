// SPDX-License-Identifier: AGPL-3.0-or-later
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
pub fn exchange<S: Read + Write + ?Sized>(stream: &mut S) -> Result<Vec<Identity>, AgentError> {
    let message = ask(stream, &request_identities())?;
    match message.split_first() {
        Some((&MSG_IDENTITIES_ANSWER, body)) => parse_identities(body),
        Some((&MSG_FAILURE, _)) => Err(AgentError::Refused),
        _ => Err(AgentError::Protocol("not an identities answer")),
    }
}

/// Writes `request` (framed) on `stream` and reads one framed answer, returned without its length.
///
/// # Errors
///
/// [`AgentError::Io`] when the stream breaks, [`AgentError::Protocol`] for an empty answer or one
/// over [`MAX_MESSAGE`].
pub fn ask<S: Read + Write + ?Sized>(
    stream: &mut S,
    request: &[u8],
) -> Result<Vec<u8>, AgentError> {
    stream
        .write_all(request)
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
    Ok(message)
}

#[cfg(test)]
mod tests;
