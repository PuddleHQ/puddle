// SPDX-License-Identifier: GPL-3.0-or-later
//! The SSH agent on this machine, as Puddle's host side sees it: where it listens and which keys it
//! holds.
//!
//! The agent is the user's own (the OpenSSH Authentication Agent, 1Password, Bitwarden or Pageant on
//! Windows; the `SSH_AUTH_SOCK` of Puddle's session on Linux). Puddle only asks it, over the agent
//! protocol of `draft-miller-ssh-agent`, for its key list. Nothing here signs, adds or removes a key.
//!
//! | Piece | What |
//! |---|---|
//! | [`AgentEndpoint`] | the named pipe or socket the agent listens on, and the rule that picks it |
//! | [`list_identities`] | one key-list request with a time limit, because an agent that asks the user first (1Password, Bitwarden) may never answer |
//! | [`Identity`] | one key the agent holds: its public blob and its comment |
//! | [`key_family`] | a key's type as a short name (`ed25519`, `rsa`), safe to show |
//!
//! The wire code ([`wire`]) is plain `Read` and `Write`, so every answer an agent can give is tested
//! without one. The request runs on its own thread: when the limit passes the caller gets
//! [`AgentError::TimedOut`] at once and the thread ends when the agent answers or closes (on Unix the
//! read has its own time limit, so it ends by itself).
#![forbid(unsafe_code)]

mod endpoint;
mod error;
mod identity;
mod list;
pub mod wire;

pub use endpoint::{AgentEndpoint, WINDOWS_DEFAULT_PIPE};
pub use error::AgentError;
pub use identity::{Identity, key_family};
pub use list::list_identities;
