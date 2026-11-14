// SPDX-License-Identifier: AGPL-3.0-or-later
//! What a spliced SSH connection's first key exchange shows in clear: the server's host key and its
//! signature over the session identifier.
//!
//! The session identifier itself is a hash over the shared secret, so nobody between client and
//! server can compute it. The signature is enough: a session bind (`session-bind@openssh.com`)
//! carries the same host key and signature, and a bind whose signature checks over its session
//! identifier and matches what was seen here is for this very session.
//!
//! The observer reads the bytes the server sends, before the first `SSH_MSG_NEWKEYS`: the
//! identification line (and any lines before it), then binary packets with no encryption and no
//! MAC (RFC 4253 sections 4.2 and 6). The reply that carries the host key is message 31
//! (`KEXDH_REPLY`, `KEX_ECDH_REPLY` and the hybrid methods) or 33 (`KEX_DH_GEX_REPLY`); each is a
//! host key, the server's public value and a signature, three strings. Message 31 of the group
//! exchange (`KEX_DH_GEX_GROUP`) has two fields and is passed over. Whatever does not parse ends the
//! observation with nothing seen: the connection itself is never touched.

use ssh_key::public::KeyData;
use ssh_key::{PublicKey, Signature};

use crate::wire;

/// Bytes read before the observer gives up: a key exchange is a few kilobytes, the largest
/// (post-quantum hybrid, 8192-bit group exchange) well under this.
pub const MAX_OBSERVED: usize = 64 * 1024;

/// The longest line before the identification (RFC 4253 section 4.2 allows 255 bytes for the
/// identification itself).
const MAX_LINE: usize = 1024;

const MSG_NEWKEYS: u8 = 21;
const MSG_KEX_REPLY: u8 = 31;
const MSG_KEX_GEX_REPLY: u8 = 33;

/// The host key and signature of a key exchange's reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeenExchange {
    /// The server's host key.
    pub host_key: KeyData,
    /// The server's signature blob over the session identifier.
    pub signature: Vec<u8>,
}

/// Reads the server's side of one connection.
#[derive(Debug, Default)]
pub struct KexObserver {
    buf: Vec<u8>,
    state: State,
}

#[derive(Debug, Default, PartialEq, Eq)]
enum State {
    #[default]
    Identification,
    Packets,
    Done,
}

impl KexObserver {
    /// A new observer, at the start of the server's bytes.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the observer wants no more bytes: it saw the reply, the end of the key exchange, or
    /// something it cannot read.
    #[must_use]
    pub fn is_done(&self) -> bool {
        self.state == State::Done
    }

    /// Feeds the next bytes the server sent. Returns the exchange when these bytes complete its
    /// reply; after that, and after anything unreadable, it returns `None` for good.
    pub fn feed(&mut self, bytes: &[u8]) -> Option<SeenExchange> {
        if self.is_done() {
            return None;
        }
        if self.buf.len() + bytes.len() > MAX_OBSERVED {
            return self.give_up();
        }
        self.buf.extend_from_slice(bytes);
        if self.state == State::Identification && !self.read_identification() {
            return None;
        }
        self.read_packets()
    }

    fn give_up(&mut self) -> Option<SeenExchange> {
        self.state = State::Done;
        self.buf = Vec::new();
        None
    }

    /// Drops whole lines until the identification line is gone. False while more bytes are needed
    /// (or after giving up).
    fn read_identification(&mut self) -> bool {
        loop {
            let Some(end) = self.buf.iter().position(|&b| b == b'\n') else {
                if self.buf.len() > MAX_LINE {
                    self.give_up();
                }
                return false;
            };
            if end > MAX_LINE {
                self.give_up();
                return false;
            }
            let is_identification = self.buf.starts_with(b"SSH-");
            self.buf.drain(..=end);
            if is_identification {
                self.state = State::Packets;
                return true;
            }
        }
    }

    fn read_packets(&mut self) -> Option<SeenExchange> {
        while let Some((&len, rest)) = self.buf.split_first_chunk::<4>() {
            let len = u32::from_be_bytes(len) as usize;
            if len > MAX_OBSERVED {
                return self.give_up();
            }
            let packet = rest.get(..len)?;
            let Some((&padding, body)) = packet.split_first() else {
                return self.give_up();
            };
            let Some(payload) = body
                .len()
                .checked_sub(usize::from(padding))
                .and_then(|n| body.get(..n))
            else {
                return self.give_up();
            };
            match payload.first() {
                Some(&MSG_NEWKEYS) | None => return self.give_up(),
                Some(&(MSG_KEX_REPLY | MSG_KEX_GEX_REPLY)) => {
                    if let Some(seen) = reply(payload.get(1..).unwrap_or_default()) {
                        self.give_up();
                        return Some(seen);
                    }
                }
                Some(_) => {}
            }
            self.buf.drain(..4 + len);
        }
        None
    }
}

/// A reply's three strings: host key, the server's public value, signature. `None` for any other
/// shape (the group exchange's group message, say).
fn reply(mut body: &[u8]) -> Option<SeenExchange> {
    let host_key = wire::take_string(&mut body).ok()?;
    let _server_value = wire::take_string(&mut body).ok()?;
    let signature = wire::take_string(&mut body).ok()?;
    if !body.is_empty() {
        return None;
    }
    let host_key = PublicKey::from_bytes(host_key).ok()?.key_data().clone();
    Signature::try_from(signature).ok()?;
    Some(SeenExchange {
        host_key,
        signature: signature.to_vec(),
    })
}

#[cfg(test)]
mod tests;
