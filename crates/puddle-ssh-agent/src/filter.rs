// SPDX-License-Identifier: AGPL-3.0-or-later
//! The filtered agent: what the guest's agent socket answers, one client connection at a time.
//!
//! A client must first bind its connection to a session (`session-bind@openssh.com`, which
//! OpenSSH's `ssh` sends before it lists keys). The bind holds only when the session's key exchange
//! was seen in clear on one of the workspace's own spliced connections ([`Context::seen`]), to a
//! host the user turned the agent on for, and the host key is not a Git host's. After that the key
//! list holds only the keys allowed for that host, and a signature is given only over a login to
//! that very session with one of those keys. Everything else is refused: a list or a signature
//! before a bind, a second bind, a bind for agent forwarding, an `SSHSIG` (commit or file signing),
//! adding, removing or locking keys and every other extension. Each refusal is the protocol's
//! failure answer, so the client goes on as with an agent that has no key for it.

use std::collections::BTreeSet;
use std::fmt;

use ssh_key::public::KeyData;
use ssh_key::{HashAlg, PublicKey};

use crate::bind::SessionBind;
use crate::error::AgentError;
use crate::identity::Identity;
use crate::request::{self, KNOWN_SIGN_FLAGS, Request, SESSION_BIND};
use crate::userauth::{self, Signed};
use crate::{git_hosts, key_family};

/// Which of the agent's keys a host may use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AllowedKeys {
    /// Every key the agent holds.
    All,
    /// Only the keys with these SHA-256 fingerprints (`SHA256:...`, as `ssh-keygen -l` prints them).
    Only(BTreeSet<String>),
}

impl AllowedKeys {
    fn allows(&self, key: &[u8]) -> bool {
        match self {
            Self::All => true,
            Self::Only(fingerprints) => fingerprint(key).is_some_and(|f| fingerprints.contains(&f)),
        }
    }
}

/// A key blob's SHA-256 fingerprint, or `None` for a blob that is not a key.
#[must_use]
pub fn fingerprint(key: &[u8]) -> Option<String> {
    PublicKey::from_bytes(key)
        .ok()
        .map(|k| k.fingerprint(HashAlg::Sha256).to_string())
}

/// What the filter asks of Puddle: the workspace's spliced sessions, the user's switches and the
/// host's agent.
pub trait Context {
    /// The host name of a live spliced connection of this workspace whose first key exchange
    /// carried `host_key` and `signature`.
    fn seen(&self, host_key: &KeyData, signature: &[u8]) -> Option<String>;
    /// The keys `host` may use, or `None` when the agent is off for it.
    fn allowed(&self, host: &str) -> Option<AllowedKeys>;
    /// The host agent's key list.
    ///
    /// # Errors
    ///
    /// Whatever the host's agent fails with.
    fn list(&self) -> Result<Vec<Identity>, AgentError>;
    /// A signature from the host's agent.
    ///
    /// # Errors
    ///
    /// Whatever the host's agent fails with.
    fn sign(&self, key: &[u8], data: &[u8], flags: u32) -> Result<Vec<u8>, AgentError>;
}

/// Why a request was refused (for the log; the client only sees the failure answer).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The message is not agent protocol.
    Malformed,
    /// A key list or a signature before a session bind.
    NotBound,
    /// A second bind on one connection.
    SecondBind,
    /// A bind for agent forwarding: the guest may not pass the agent on.
    Forwarding,
    /// The host key is a Git host's: Puddle ends SSH to those itself.
    GitHostKey,
    /// No spliced connection of this workspace showed this key exchange.
    UnseenSession,
    /// The agent is off for the host of the session.
    AgentOff(String),
    /// The bind's signature does not check over its session identifier.
    BadBindSignature,
    /// An `SSHSIG` request: commit or file signing.
    SshSig,
    /// The data to sign is not a login.
    NotLogin,
    /// A login for another session or another server than the bound one.
    OtherSession,
    /// The key is not allowed for the host, or not one the login names.
    KeyNotAllowed,
    /// Sign flags the protocol does not define.
    UnknownFlags(u32),
    /// An extension other than the session bind.
    Extension,
    /// Adding, removing or locking keys and every other request type.
    Unsupported(u8),
    /// The host's agent failed.
    Agent(String),
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed => f.write_str("not agent protocol"),
            Self::NotBound => f.write_str("no session bind on this connection"),
            Self::SecondBind => f.write_str("a second session bind"),
            Self::Forwarding => f.write_str("a bind for agent forwarding"),
            Self::GitHostKey => f.write_str("a Git host's key"),
            Self::UnseenSession => f.write_str("a session no spliced connection showed"),
            Self::AgentOff(host) => write!(f, "the agent is off for {host}"),
            Self::BadBindSignature => f.write_str("the bind's signature does not check"),
            Self::SshSig => f.write_str("an SSHSIG signature"),
            Self::NotLogin => f.write_str("data that is not a login"),
            Self::OtherSession => f.write_str("a login for another session"),
            Self::KeyNotAllowed => f.write_str("a key not allowed for the host"),
            Self::UnknownFlags(flags) => write!(f, "sign flags {flags:#x}"),
            Self::Extension => f.write_str("an extension other than the session bind"),
            Self::Unsupported(kind) => write!(f, "request type {kind}"),
            Self::Agent(why) => write!(f, "the host's agent: {why}"),
        }
    }
}

/// What one request came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// A session bind held, for this host.
    Bound(String),
    /// The key list went out with this many keys.
    Listed(usize),
    /// A signature went out, from a key of this family (`ed25519`, `rsa`).
    Signed(&'static str),
    /// Refused.
    Refused(Refusal),
}

struct Bound {
    host: String,
    host_key: KeyData,
    session_id: Vec<u8>,
}

/// One client connection's filter.
#[derive(Default)]
pub struct Filter {
    bound: Option<Bound>,
    binds: usize,
}

impl Filter {
    /// A filter for a new connection: nothing bound.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Answers one request (`message`, unframed). Returns the framed answer and what happened.
    pub fn answer(&mut self, message: &[u8], cx: &impl Context) -> (Vec<u8>, Outcome) {
        let outcome = match request::parse(message) {
            Ok(Request::List) => return self.list(cx),
            Ok(Request::Sign { key, data, flags }) => return self.sign(&key, &data, flags, cx),
            Ok(Request::Extension { name, body }) if name == SESSION_BIND => self.bind(&body, cx),
            Ok(Request::Extension { .. }) => Outcome::Refused(Refusal::Extension),
            Ok(Request::Other(kind)) => Outcome::Refused(Refusal::Unsupported(kind)),
            Err(_) => Outcome::Refused(Refusal::Malformed),
        };
        let answer = match outcome {
            Outcome::Bound(_) => request::success(),
            _ => request::failure(),
        };
        (answer, outcome)
    }

    fn bind(&mut self, body: &[u8], cx: &impl Context) -> Outcome {
        self.binds += 1;
        if self.binds > 1 {
            // A refused first bind leaves the connection unbound for good, too.
            self.bound = None;
            return Outcome::Refused(Refusal::SecondBind);
        }
        let Ok(bind) = SessionBind::parse(body) else {
            return Outcome::Refused(Refusal::Malformed);
        };
        if bind.forwarding {
            return Outcome::Refused(Refusal::Forwarding);
        }
        if git_hosts::is_git_host_key(&bind.host_key) {
            return Outcome::Refused(Refusal::GitHostKey);
        }
        let Some(host) = cx.seen(&bind.host_key, &bind.signature) else {
            return Outcome::Refused(Refusal::UnseenSession);
        };
        if cx.allowed(&host).is_none() {
            return Outcome::Refused(Refusal::AgentOff(host));
        }
        if !bind.is_signed() {
            return Outcome::Refused(Refusal::BadBindSignature);
        }
        self.bound = Some(Bound {
            host: host.clone(),
            host_key: bind.host_key,
            session_id: bind.session_id,
        });
        Outcome::Bound(host)
    }

    fn list(&self, cx: &impl Context) -> (Vec<u8>, Outcome) {
        let Some(bound) = &self.bound else {
            // An empty list, not a failure: a client that never binds (an old `ssh`, another
            // tool) goes on to its other ways of logging in.
            return (
                request::identities(&[]),
                Outcome::Refused(Refusal::NotBound),
            );
        };
        // Asked again on every request: a switch turned off acts on bound connections at once.
        let Some(allowed_keys) = cx.allowed(&bound.host) else {
            return (
                request::identities(&[]),
                Outcome::Refused(Refusal::AgentOff(bound.host.clone())),
            );
        };
        match cx.list() {
            Ok(keys) => {
                let allowed: Vec<&Identity> = keys
                    .iter()
                    .filter(|k| allowed_keys.allows(k.blob()))
                    .collect();
                (
                    request::identities(&allowed),
                    Outcome::Listed(allowed.len()),
                )
            }
            Err(e) => refused(Refusal::Agent(e.to_string())),
        }
    }

    fn sign(&self, key: &[u8], data: &[u8], flags: u32, cx: &impl Context) -> (Vec<u8>, Outcome) {
        let Some(bound) = &self.bound else {
            return refused(Refusal::NotBound);
        };
        if flags & !KNOWN_SIGN_FLAGS != 0 {
            return refused(Refusal::UnknownFlags(flags));
        }
        let (session_id, login_key, host_key) = match userauth::read(data) {
            Signed::Login {
                session_id,
                key,
                host_key,
            } => (session_id, key, host_key),
            Signed::SshSig => return refused(Refusal::SshSig),
            Signed::Other => return refused(Refusal::NotLogin),
        };
        let other_server = host_key.is_some_and(|k| !same_key(k, &bound.host_key));
        if session_id != bound.session_id || other_server {
            return refused(Refusal::OtherSession);
        }
        let Some(allowed_keys) = cx.allowed(&bound.host) else {
            return refused(Refusal::AgentOff(bound.host.clone()));
        };
        if login_key != key || !allowed_keys.allows(key) {
            return refused(Refusal::KeyNotAllowed);
        }
        match cx.sign(key, data, flags) {
            Ok(signature) => {
                let family = PublicKey::from_bytes(key)
                    .map_or("other", |k| key_family(k.algorithm().as_str()));
                (request::signature(&signature), Outcome::Signed(family))
            }
            Err(e) => refused(Refusal::Agent(e.to_string())),
        }
    }

    /// The host this connection is bound to, once a bind held.
    #[must_use]
    pub fn host(&self) -> Option<&str> {
        self.bound.as_ref().map(|b| b.host.as_str())
    }
}

fn refused(why: Refusal) -> (Vec<u8>, Outcome) {
    (request::failure(), Outcome::Refused(why))
}

fn same_key(blob: &[u8], key: &KeyData) -> bool {
    PublicKey::from_bytes(blob).is_ok_and(|k| k.key_data() == key)
}

#[cfg(test)]
mod tests;
