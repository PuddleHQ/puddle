// SPDX-License-Identifier: AGPL-3.0-or-later
//! What the proxy does with one `CONNECT`, decided from one read of the workspace's termination.
//!
//! [`Proxy::seen`] reads the termination registry once; [`ConnectionPlan::new`] turns that
//! snapshot, the request and what the guest said about it ([`GuestHints`]) into one verdict,
//! without I/O. Nothing after it asks the registry again, so the rules on method and path, the
//! decrypted set and the Git hosts of one connection always come from the same termination.

use std::sync::Arc;

use puddle_types::{EgressRequest, Host, PassThroughCode, ProtocolHint};

use crate::http::Head;
use crate::ssh;
use crate::ssh_git::{GitHost, GitSsh, PathForm};
use crate::terminate::{self, Mode, RequestRules, Termination};

use super::{Proxy, RecordChange, Refusal, refuse_by_request_rule};

/// The port SSH listens on.
const GIT_SSH_PORT: u16 = 22;

/// The workspace's termination as the proxy saw it at the start of a connection.
#[derive(Debug, Clone)]
pub(crate) enum Seen {
    /// The proxy decrypts nothing (it was made without [`Proxy::with_termination`]).
    Unterminated,
    /// The proxy decrypts, and the host holds no termination for this workspace: it believes the
    /// workspace is not running.
    NotRunning,
    /// The workspace's termination.
    Running(Arc<Termination>),
}

impl Seen {
    /// The termination, when the workspace has one.
    pub(crate) fn termination(&self) -> Option<&Arc<Termination>> {
        match self {
            Self::Running(termination) => Some(termination),
            Self::Unterminated | Self::NotRunning => None,
        }
    }

    /// The workspace's rules on method and path, when it has a termination.
    pub(crate) fn request_rules(&self) -> Option<&RequestRules> {
        self.termination().map(|t| t.request_rules())
    }

    /// Whether `host` is a name whose SSH Puddle serves itself or refuses, never splices.
    pub(crate) fn is_git_ssh_name(&self, host: &Host) -> bool {
        is_git_ssh_name(self.termination().map(Arc::as_ref), host)
    }

    /// How a connection to `request` is decrypted, or `None` when it is spliced: a name of the
    /// workspace's termination set (or every name, when it decrypts every host) on port 443.
    pub(crate) fn mode(&self, request: &EgressRequest, from_container: bool) -> Option<Mode> {
        if request.port != terminate::request::HTTPS_PORT {
            return None;
        }
        self.termination()?.mode_for(&request.host, from_container)
    }

    /// Why a host the workspace would decrypt (it decrypts every host) is passed through instead.
    pub(crate) fn skipped(
        &self,
        request: &EgressRequest,
        from_container: bool,
    ) -> Option<PassThroughCode> {
        if request.port != terminate::request::HTTPS_PORT {
            return None;
        }
        self.termination()?.skipped(&request.host, from_container)
    }
}

/// Whether `host` is a built-in Git host or a host of one of the workspace's Git credentials.
pub(crate) fn is_git_ssh_name(termination: Option<&Termination>, host: &Host) -> bool {
    crate::ssh_git::built_in(host).is_some()
        || termination.is_some_and(|t| t.git_ssh_hosts().contains(host))
}

impl Proxy {
    /// The one read of `workspace`'s termination a connection makes.
    pub(crate) fn seen(&self, workspace: &puddle_types::WorkspaceId) -> Seen {
        let Some(terminations) = self.terminations.as_ref() else {
            return Seen::Unterminated;
        };
        terminations
            .source
            .termination(workspace)
            .map_or(Seen::NotRunning, Seen::Running)
    }
}

/// What the guest said about its own connection. Both are the guest's word, so only
/// [`ConnectionPlan::new`] reads them, and only after the host-set decision: a host the
/// workspace decrypts for a credential, secret or rule is decrypted whatever they say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GuestHints {
    /// The request announced SSH, or its first bytes were an SSH identification line.
    ssh: bool,
    /// The agent marked the client as in a container (PX-65).
    from_container: bool,
}

impl GuestHints {
    /// The hints of a `CONNECT` whose head is `head`.
    pub(crate) fn of(request: &EgressRequest, head: &Head) -> Self {
        Self {
            ssh: request.protocol == Some(ProtocolHint::Ssh),
            from_container: ssh::from_container(head),
        }
    }
}

/// Where a spliced connection goes and what the splice watches.
pub(crate) struct Splice {
    /// Why a host the workspace would decrypt is passed through, for the audit.
    pub(crate) passed_through: Option<PassThroughCode>,
    /// The termination whose token endpoints and agent hosts the splice watches.
    pub(crate) termination: Option<Arc<Termination>>,
    /// SSH in the tunnel is refused: the name is one whose SSH Puddle serves or refuses.
    pub(crate) git_ssh_name: bool,
}

/// What the proxy does with one `CONNECT` whose destination is known.
pub(crate) enum ConnectionPlan<'p> {
    /// Puddle's own SSH server for a Git host (no admission: its upstream leg is admitted).
    GitSsh {
        git: &'p GitSsh,
        termination: Arc<Termination>,
        host: GitHost,
    },
    /// SSH to a Git host's name that Puddle does not serve: refused before the rules.
    SshRefused,
    /// Admitted by the rules and the address guard first, then this.
    Admit(Admitted<'p>),
}

/// What an admitted `CONNECT` does.
pub(crate) enum Admitted<'p> {
    /// A tunnel to a port other than 443 of a host with rules on method and path, whose requests
    /// the rules could never see.
    RuleRefusesPort(Refusal, RecordChange),
    /// Decrypted.
    Terminate {
        termination: Arc<Termination>,
        mode: Mode,
        tls: &'p puddle_upstream::TlsClient,
        /// The guest announced SSH: its first bytes decide, and an identification line is
        /// spliced as `splice` says.
        ssh_first: bool,
        splice: Splice,
    },
    /// Spliced.
    Splice(Splice),
}

impl<'p> ConnectionPlan<'p> {
    /// The plan for `request` (head `head`) from `seen`. No I/O.
    pub(crate) fn new(
        proxy: &'p Proxy,
        seen: &Seen,
        request: &EgressRequest,
        head: &Head,
        hints: GuestHints,
    ) -> Self {
        let termination = seen.termination();
        // A client that waits for the server's identification before it sends its own
        // (`ssh-keyscan`) sends nothing that marks it as SSH. Port 22 of a Git host whose SSH
        // Puddle serves is SSH, so such a client gets Puddle's server and its keys, as `ssh` does.
        if (hints.ssh || request.port == GIT_SSH_PORT)
            && let Some(git) = proxy.git_ssh.as_ref()
            && let Some(termination) = termination
            && let Some(host) = git_host(termination, &request.host)
        {
            return Self::GitSsh {
                git,
                termination: Arc::clone(termination),
                host,
            };
        }
        if hints.ssh && seen.is_git_ssh_name(&request.host) {
            return Self::SshRefused;
        }
        if let Some((refusal, change)) = refuse_by_request_rule(seen, request, head, None) {
            return Self::Admit(Admitted::RuleRefusesPort(refusal, change));
        }
        let splice = |passed_through| Splice {
            passed_through,
            termination: termination.cloned(),
            git_ssh_name: seen.is_git_ssh_name(&request.host),
        };
        if let Some(mode) = seen.mode(request, hints.from_container)
            && let (Some(termination), Some(config)) = (termination, proxy.terminations.as_ref())
        {
            return Self::Admit(Admitted::Terminate {
                termination: Arc::clone(termination),
                mode,
                tls: &config.tls,
                ssh_first: hints.ssh,
                // A decrypted host has no pass-through reason (`Termination::skipped`).
                splice: splice(None),
            });
        }
        Self::Admit(Admitted::Splice(splice(
            seen.skipped(request, hints.from_container),
        )))
    }
}

/// The Git host Puddle serves SSH for at `name`: a built-in one, or a host of one of the
/// workspace's Git credentials.
fn git_host(termination: &Termination, name: &Host) -> Option<GitHost> {
    crate::ssh_git::built_in(name).or_else(|| {
        termination.git_ssh_hosts().contains(name).then(|| GitHost {
            https: name.clone(),
            form: PathForm::Plain,
        })
    })
}

#[cfg(test)]
mod tests;
