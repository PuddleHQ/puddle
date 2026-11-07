// SPDX-License-Identifier: GPL-3.0-or-later
//! Git over SSH, the guest's side: Puddle's own SSH server for the Git hosts it terminates.
//!
//! A workspace's SSH to a Git host (`github.com`, `ssh.dev.azure.com`, the host of an identity's
//! credential) is not spliced to the real server. Puddle plays the server on that connection: it
//! answers with its own host keys, accepts the client at its first authentication attempt (the
//! route says which workspace it is), reads each `git-upload-pack` or `git-receive-pack` command
//! with one strict grammar ([`grammar`]), asks the workspace's injector whether the repository
//! may be read or written (the same table and the same refusal words as an HTTPS request, so a
//! push to a repository that is not on the list ends here), and only then hands the stream to a
//! [`GitSshUpstream`]. The upstream is the seam: how Puddle reaches the Git host with the
//! workspace's credential is not this module's business, and the guest never holds a key.
//!
//! Without an upstream ([`crate::Proxy::with_git_ssh`] not called) nothing here runs and SSH is
//! refused as before (`ssh_unsupported`).
//!
//! What a session may do: up to ten session channels at once, each with one
//! `exec` of a grammar command (and an optional `GIT_PROTOCOL` `env` before it) or one `shell`
//! with no terminal (`ssh -T`). A terminal, a subsystem, a forward either way, agent and X11
//! forwarding, a second `exec` and any other `env` are refused on the channel.
//!
//! An SSH connection is one connection record: the first refused command, else the first command,
//! as service and repository, with the credential's binding when the table let it through.

mod grammar;
mod host_keys;
mod session;

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use puddle_types::Host;
use tokio::io::{AsyncRead, AsyncWrite};

use crate::destination::BoxFuture;
pub use grammar::GitService;
pub(crate) use grammar::{GitHost, PathForm, built_in, built_in_names};
pub use host_keys::{GitSshHostKeys, GitSshKeyError};
pub(crate) use session::serve;

/// The SSH names of the built-in Git hosts (`github.com`, `ssh.github.com`, `ssh.dev.azure.com`
/// and the rest), which every workspace's SSH to is ended by Puddle once Git over SSH is on.
#[must_use]
pub fn git_ssh_built_in_hosts() -> Vec<Host> {
    built_in_names()
}

/// The host key type a [`GitSsh`] serves; the keys live in memory for one start of a workspace.
pub use russh::keys::PrivateKey as GitSshHostKey;

/// The stream of one `git-upload-pack` or `git-receive-pack`: what the guest's Git writes to the
/// command's input comes out of its reads, what is written to it is the command's output. It ends
/// (reads `0` bytes) when the guest's side is done.
pub trait GitSshStream: AsyncRead + AsyncWrite + Send + Unpin {}

impl<T: AsyncRead + AsyncWrite + Send + Unpin> GitSshStream for T {}

/// A command the table has allowed, for the upstream to carry out.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct GitSshCommand {
    /// The HTTPS host the repository is on (`github.com` for `ssh.github.com`; for Azure DevOps'
    /// `vs-ssh.visualstudio.com`, the organisation's own `org.visualstudio.com`).
    pub host: Host,
    /// Fetch or push.
    pub service: GitService,
    /// The repository's HTTPS path without an endpoint, canonical: `/acme/web.git`,
    /// `/org/project/_git/repo`. `<path>/info/refs?service=...` is the smart-HTTP advertisement.
    pub path: String,
    /// The `GIT_PROTOCOL` the guest sent, validated (`version=2`, `version=1:version=2`), or
    /// `None` for protocol version 0.
    pub git_protocol: Option<String>,
    /// The credential binding the table chose for the repository, by id (never the credential);
    /// `None` when no identity covers it.
    pub binding_id: Option<String>,
}

/// Why an upstream could not carry out a command. The message goes to the guest as one line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitSshFailure {
    /// One line, without `puddle: `.
    pub message: String,
}

impl GitSshFailure {
    /// A failure that says `message`.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// What `ssh -T` prints and the exit status it ends with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitSshGreeting {
    /// The text, on the error stream.
    pub message: String,
    /// The exit status (1 on GitHub, 255 for a failed login).
    pub status: u32,
}

/// How Puddle reaches a Git host for a terminated SSH session.
pub trait GitSshUpstream: Send + Sync + fmt::Debug {
    /// Carries out `command` between `io` and the Git host. The table has allowed it already.
    /// `Ok` is exit status 0; an error is one line for the guest and exit status 1. Dropping
    /// `io` ends the guest's channel.
    fn run<'a>(
        &'a self,
        command: &'a GitSshCommand,
        io: &'a mut dyn GitSshStream,
    ) -> BoxFuture<'a, Result<(), GitSshFailure>>;

    /// The answer to a `shell` request on `host`. The default is the line a workspace with no
    /// identity for the host gets.
    fn greeting<'a>(&'a self, host: &'a Host) -> BoxFuture<'a, GitSshGreeting> {
        Box::pin(std::future::ready(GitSshGreeting {
            message: format!(
                "puddle: no identity on this workspace has a credential for {host}; add one on the workspace's Git tab\n"
            ),
            status: 255,
        }))
    }
}

/// How long the proxy waits at each stage of a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GitSshLimits {
    /// A guest that has not finished its handshake and authentication in this time is closed.
    pub handshake: Duration,
    /// A connection with no open channel is closed after this long.
    pub idle: Duration,
    /// Session channels open at once on one connection.
    pub channels: usize,
}

impl Default for GitSshLimits {
    fn default() -> Self {
        Self {
            handshake: Duration::from_secs(60),
            idle: Duration::from_secs(600),
            channels: 10,
        }
    }
}

/// Git over SSH for terminated Git hosts: the upstream and the host keys. Set with
/// [`crate::Proxy::with_git_ssh`].
#[derive(Clone)]
pub struct GitSsh {
    pub(crate) upstream: Arc<dyn GitSshUpstream>,
    pub(crate) host_keys: Arc<Vec<GitSshHostKey>>,
    pub(crate) limits: GitSshLimits,
}

impl fmt::Debug for GitSsh {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GitSsh")
            .field("upstream", &self.upstream)
            .field("host_keys", &self.host_keys.len())
            .field("limits", &self.limits)
            .finish()
    }
}

impl GitSsh {
    /// Serves the SSH of terminated Git hosts, carrying commands out through `upstream`, with
    /// `host_keys` for a workspace whose [`crate::Termination`] has no keys of its own
    /// ([`crate::Termination::with_git_ssh_keys`]); empty when every workspace has its own.
    #[must_use]
    pub fn new(upstream: Arc<dyn GitSshUpstream>, host_keys: Vec<GitSshHostKey>) -> Self {
        Self {
            upstream,
            host_keys: Arc::new(host_keys),
            limits: GitSshLimits::default(),
        }
    }

    /// Uses `limits` instead of the defaults.
    #[must_use]
    pub fn with_limits(mut self, limits: GitSshLimits) -> Self {
        self.limits = limits;
        self
    }
}
