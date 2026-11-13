// SPDX-License-Identifier: AGPL-3.0-or-later
//! The SSH agent on this machine, as Puddle's host side sees it, and the filtered agent a workspace
//! may be given in front of it.
//!
//! The agent is the user's own (the OpenSSH Authentication Agent, 1Password, Bitwarden or Pageant on
//! Windows; the `SSH_AUTH_SOCK` of Puddle's session on Linux). Puddle asks it, over the agent
//! protocol of `draft-miller-ssh-agent`, for its key list and, through the filter, for a signature
//! over one login. Nothing here adds, removes or locks a key.
//!
//! | Piece | What |
//! |---|---|
//! | [`AgentEndpoint`] | the named pipe or socket the agent listens on, and the rule that picks it |
//! | [`list_identities`] | one key-list request with a time limit, because an agent that asks the user first (1Password, Bitwarden) may never answer |
//! | [`sign`] | one sign request with a time limit |
//! | [`Identity`] | one key the agent holds: its public blob and its comment |
//! | [`key_family`] | a key's type as a short name (`ed25519`, `rsa`), safe to show |
//! | [`filter`] | what a guest's agent socket answers: keys and signatures only for a login to a host the user allowed, over a session Puddle saw |
//! | [`kex`] | the host key and signature a spliced SSH connection's key exchange shows in clear |
//! | [`is_git_host_key`] | the Git hosts' published host keys, which the filter never signs a login to |
//! | [`request`] | the agent protocol from the agent's side: requests in, answers out |
//!
//! The wire code ([`wire`], [`request`]) is plain bytes, `Read` and `Write`, so every answer an agent
//! can give is tested without one. A request to the host's agent runs on its own thread: when the
//! limit passes the caller gets [`AgentError::TimedOut`] at once and the thread ends when the agent
//! answers or closes (on Unix the read has its own time limit, so it ends by itself).
#![forbid(unsafe_code)]

mod bind;
mod endpoint;
mod error;
pub mod filter;
mod git_hosts;
mod identity;
pub mod kex;
mod list;
mod parked;
pub mod request;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
mod userauth;
pub mod wire;

pub use bind::SessionBind;
pub use endpoint::{AgentEndpoint, WINDOWS_DEFAULT_PIPE};
pub use error::AgentError;
pub use git_hosts::is_git_host_key;
pub use identity::{Identity, key_family};
pub use list::{list_identities, sign};
