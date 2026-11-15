// SPDX-License-Identifier: AGPL-3.0-or-later
//! The `session-bind@openssh.com` extension: before it lists keys, OpenSSH's `ssh` tells the agent
//! which session it logs in to, by the server's host key, the session identifier and the server's
//! signature over that identifier from the key exchange.

use ssh_key::public::KeyData;
use ssh_key::{PublicKey, Signature};

use crate::error::AgentError;
use crate::wire;

/// OpenSSH's own limit on a session identifier (the hash of the key exchange; 64 bytes for SHA-512).
const MAX_SESSION_ID: usize = 128;

/// One parsed bind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionBind {
    /// The server's host key, parsed.
    pub host_key: KeyData,
    /// The session identifier the client says it has.
    pub session_id: Vec<u8>,
    /// The server's signature over the session identifier, as the key exchange carried it.
    pub signature: Vec<u8>,
    /// Whether the client is forwarding the agent to that server rather than logging in to it.
    pub forwarding: bool,
}

impl SessionBind {
    /// Reads the extension's fields (what follows its name).
    ///
    /// # Errors
    ///
    /// [`AgentError::Protocol`] when a field is cut short or not what it should be (a host key that
    /// does not parse, a session identifier over 128 bytes, a flag other than 0 or 1), or
    /// bytes are left over.
    pub fn parse(mut body: &[u8]) -> Result<Self, AgentError> {
        let host_key = wire::take_string(&mut body)?;
        let host_key = PublicKey::from_bytes(host_key)
            .map_err(|_| AgentError::Protocol("the bind's host key is not a key"))?
            .key_data()
            .clone();
        let session_id = wire::take_string(&mut body)?.to_vec();
        if session_id.is_empty() || session_id.len() > MAX_SESSION_ID {
            return Err(AgentError::Protocol(
                "the bind's session id has a bad length",
            ));
        }
        let signature = wire::take_string(&mut body)?.to_vec();
        let forwarding = match body {
            [0] => false,
            [1] => true,
            _ => return Err(AgentError::Protocol("the bind does not end with one flag")),
        };
        Ok(Self {
            host_key,
            session_id,
            signature,
            forwarding,
        })
    }

    /// Whether the signature is the host key's over the session identifier. Only then is the
    /// session identifier the one of the key exchange that carried this signature.
    #[must_use]
    pub fn is_signed(&self) -> bool {
        let Ok(signature) = Signature::try_from(self.signature.as_slice()) else {
            return false;
        };
        signature::Verifier::verify(&self.host_key, &self.session_id, &signature).is_ok()
    }
}

#[cfg(test)]
mod tests;
