// SPDX-License-Identifier: AGPL-3.0-or-later
//! The agent protocol from the agent's side (`draft-miller-ssh-agent`): the requests a client sends
//! and the answers the filtered agent gives. Every request comes from the guest, so the parsers
//! assume nothing about it: a field cut short, bytes left over or an unknown type is a refusal.

use crate::error::AgentError;
use crate::identity::Identity;
use crate::wire;

const MSG_FAILURE: u8 = 5;
const MSG_SUCCESS: u8 = 6;
const MSG_REQUEST_IDENTITIES: u8 = 11;
const MSG_IDENTITIES_ANSWER: u8 = 12;
const MSG_SIGN_REQUEST: u8 = 13;
const MSG_SIGN_RESPONSE: u8 = 14;
const MSG_EXTENSION: u8 = 27;

/// The extension OpenSSH's `ssh` sends before it lists keys, naming the session it logs in to.
pub const SESSION_BIND: &[u8] = b"session-bind@openssh.com";

/// The sign-request flags a client may set: `SSH_AGENT_RSA_SHA2_256` (2) and
/// `SSH_AGENT_RSA_SHA2_512` (4). Any other bit is refused.
pub const KNOWN_SIGN_FLAGS: u32 = 0b110;

/// One request from a client, read from one message (the type byte and its body).
#[derive(Debug, PartialEq, Eq)]
pub enum Request {
    /// `SSH_AGENTC_REQUEST_IDENTITIES`.
    List,
    /// `SSH_AGENTC_SIGN_REQUEST`: the key's public blob, the data to sign and the flags.
    Sign {
        /// The public key blob the client wants a signature from.
        key: Vec<u8>,
        /// What the client wants signed.
        data: Vec<u8>,
        /// The flags (RSA hash choice).
        flags: u32,
    },
    /// `SSH_AGENTC_EXTENSION` with a name; `body` is what follows the name.
    Extension {
        /// The extension's name.
        name: Vec<u8>,
        /// The extension's own fields.
        body: Vec<u8>,
    },
    /// Any other type: adding, removing or locking keys, smart cards, or a type the protocol has
    /// not defined.
    Other(u8),
}

/// Reads one request from `message` (unframed: type byte first).
///
/// # Errors
///
/// [`AgentError::Protocol`] for an empty message, a field cut short or bytes left over.
pub fn parse(message: &[u8]) -> Result<Request, AgentError> {
    let Some((&kind, mut body)) = message.split_first() else {
        return Err(AgentError::Protocol("an empty message"));
    };
    let request = match kind {
        MSG_REQUEST_IDENTITIES => Request::List,
        MSG_SIGN_REQUEST => {
            let key = wire::take_string(&mut body)?.to_vec();
            let data = wire::take_string(&mut body)?.to_vec();
            let flags = take_u32(&mut body)?;
            Request::Sign { key, data, flags }
        }
        MSG_EXTENSION => {
            let name = wire::take_string(&mut body)?.to_vec();
            let rest = std::mem::take(&mut body).to_vec();
            Request::Extension { name, body: rest }
        }
        other => return Ok(Request::Other(other)),
    };
    if body.is_empty() {
        Ok(request)
    } else {
        Err(AgentError::Protocol(
            "the request has bytes after its fields",
        ))
    }
}

fn take_u32(buf: &mut &[u8]) -> Result<u32, AgentError> {
    let Some((head, tail)) = buf.split_first_chunk::<4>() else {
        return Err(AgentError::Protocol("a number is cut short"));
    };
    *buf = tail;
    Ok(u32::from_be_bytes(*head))
}

/// The failure answer, framed.
#[must_use]
pub fn failure() -> Vec<u8> {
    framed(&[MSG_FAILURE])
}

/// The success answer, framed.
#[must_use]
pub fn success() -> Vec<u8> {
    framed(&[MSG_SUCCESS])
}

/// A key list answer, framed. The comments go out empty: a comment is the user's own note (often a
/// path or an email address) and the guest needs only the key.
#[must_use]
pub fn identities(keys: &[&Identity]) -> Vec<u8> {
    let mut body = vec![MSG_IDENTITIES_ANSWER];
    put_u32(&mut body, keys.len());
    for key in keys {
        put_string(&mut body, key.blob());
        put_string(&mut body, b"");
    }
    framed(&body)
}

/// A sign answer, framed, holding `signature` as the agent wrote it.
#[must_use]
pub fn signature(signature: &[u8]) -> Vec<u8> {
    let mut body = vec![MSG_SIGN_RESPONSE];
    put_string(&mut body, signature);
    framed(&body)
}

/// The sign request to send on to the host's agent, framed.
#[must_use]
pub fn sign_request(key: &[u8], data: &[u8], flags: u32) -> Vec<u8> {
    let mut body = vec![MSG_SIGN_REQUEST];
    put_string(&mut body, key);
    put_string(&mut body, data);
    body.extend_from_slice(&flags.to_be_bytes());
    framed(&body)
}

/// Reads a sign answer from the host's agent (unframed) and returns the signature blob.
///
/// # Errors
///
/// [`AgentError::Refused`] for a failure answer, [`AgentError::Protocol`] for anything that is not
/// one sign answer.
pub fn parse_signature(message: &[u8]) -> Result<Vec<u8>, AgentError> {
    match message.split_first() {
        Some((&MSG_SIGN_RESPONSE, mut body)) => {
            let signature = wire::take_string(&mut body)?.to_vec();
            if body.is_empty() {
                Ok(signature)
            } else {
                Err(AgentError::Protocol(
                    "the answer has bytes after its signature",
                ))
            }
        }
        Some((&MSG_FAILURE, _)) => Err(AgentError::Refused),
        _ => Err(AgentError::Protocol("not a sign response")),
    }
}

/// Every field written here is far below 4 GiB: a message is at most 256 KiB long and the
/// answers repeat what an agent or a client sent within that limit.
fn put_u32(out: &mut Vec<u8>, n: usize) {
    out.extend_from_slice(&u32::try_from(n).unwrap_or(u32::MAX).to_be_bytes());
}

fn put_string(out: &mut Vec<u8>, bytes: &[u8]) {
    put_u32(out, bytes.len());
    out.extend_from_slice(bytes);
}

fn framed(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len() + 4);
    put_u32(&mut out, body.len());
    out.extend_from_slice(body);
    out
}

#[cfg(test)]
mod tests;
