// SPDX-License-Identifier: GPL-3.0-or-later
//! TLS termination for the hosts a workspace has a credential for.
//!
//! Almost every connection through the proxy is spliced: the guest and the real server talk TLS
//! to each other and Puddle sees only the name. A *bound* host is the exception, so that a
//! credential can be added to the request without ever entering the workspace. For those hosts, and
//! only those, the proxy is the TLS server the guest sees (with a certificate from the
//! workspace's own CA, `puddle-ca`) and a verifying TLS client to the real server.
//!
//! A connection is terminated only when all of these hold; otherwise it is spliced exactly as
//! before:
//!
//! - the workspace has a [`Termination`] (a [`TerminationSet`], its CA and an [`Injector`]);
//! - the guest asked for a *name* (an IP literal never matches) in the set;
//! - the port is 443.
//!
//! The rules, the address guard and the IP rules run first and are unchanged: terminating a host
//! never allows it.
//!
//! On a terminated connection:
//!
//! - **Order**: the guest's `ClientHello` is read first and its name checked; a client that sends
//!   another name, an IP address or none gets a TLS alert and the connection ends with the audit
//!   reason `sni_mismatch`, and nothing is sent upstream. Then the real server is connected to,
//!   offering the protocols the guest offered (`h2`, `http/1.1`), and the guest is offered what the
//!   server chose. A server that cannot be used (a certificate that is not accepted) does not stop
//!   the guest's handshake: the first request is answered with the reason.
//! - **Guest leg**: a rustls server whose one certificate is the leaf for the `CONNECT` host.
//!   HTTP/1.1 is read with the strict parser the plain-HTTP path uses, one request at a time (a
//!   pipelined request is decided on its own) and rebuilt before it is sent. HTTP/2 is served by
//!   hyper, one request per stream, with the same checks (`Host`/`:authority`, target, framing)
//!   and limits of its own (streams, header list, resets, header-read time).
//!   A client that refuses the certificate (it does not trust the workspace's CA) is recorded with
//!   the audit reason `guest_tls_rejected`; the rule's decision stands, since Puddle blocked nothing.
//!   A certificate Puddle's own CA could not make for the host is recorded as `certificate_unavailable`,
//!   and the rule's decision stands there too.
//! - **Requests**: a request for another host (`Host`, `:authority` or an absolute target) is
//!   `421`; an ambiguous one is `400`. The [`Injector`] is asked the same way on both versions.
//! - **Upstream leg**: one connection per guest connection, never shared, verified against the
//!   platform's roots plus the corporate roots with the host's clock. A certificate that is not
//!   accepted is a `502` that names the reason, and no request byte, and no credential, is sent.
//!   The [`Injector`] is asked only after the connection is verified. HTTP/2 to the server carries
//!   every stream of an HTTP/2 guest; an HTTP/2 guest talking to an HTTP/1.1 server uses a small
//!   pool of HTTP/1.1 connections owned by that guest connection.
//! - **Stand-ins**: after the injector has decided, the workspace's [`StandIns`] are swapped for
//!   their real values in the header values of the request (or, for a secret that opts in, in
//!   one named query parameter or the path), toward the hosts each is for, on both HTTP versions.
//! - **Responses**: framed by an HTTP client library, then rebuilt for the guest, trailers
//!   included. Redirects are passed through, never followed. A `401` is replaced by the
//!   injector's own refusal when the injector added the request's credential ([`Unauthorized`]);
//!   otherwise it is the server's answer like any other. The HTTP/3 entries of `Alt-Svc` are
//!   removed on both HTTP versions: a workspace has no UDP path, so a client would only try
//!   HTTP/3 and fail.
//! - **Token endpoints**: a connection that is not decrypted (another name, another port, a host
//!   decrypted only because every host is) is refused when its `ClientHello` asks for a token
//!   endpoint of the workspace's logins (audit reason `token_endpoint_spliced`): the refresh token
//!   or authorization code it would send is swapped only on a decrypted connection. This stops a
//!   careless tool, not a guest that hides the name (encrypted `ClientHello`, none, fronting).
//! - **WebSocket**: HTTP/1.1 `Upgrade` and HTTP/2 extended `CONNECT` (RFC 8441) are checked and
//!   injected like any request; once the server agrees, the two connections are a byte pipe.

mod alt_svc;
mod body;
mod exchange;
mod foreign;
mod guest;
#[cfg(test)]
pub(crate) use guest::test_client_hello;
pub(crate) use guest::{Scan, scan_hello};
mod h2;
mod handshake;
mod inject;
mod intercept;
mod leg;
pub(crate) mod request;
pub(crate) mod request_rules;
mod response;
mod session;
mod set;
mod stale;
mod stand_in;
mod streams;
mod token_body;
mod watch;
mod wiped;
mod ws;

use std::collections::HashMap;
use std::sync::{Arc, PoisonError, RwLock};

use puddle_ca::WorkspaceCa;
use puddle_ssh_agent::filter::AllowedKeys;
use puddle_types::{Host, WorkspaceId};

pub use exchange::{AnswerHead, AnswerRewriter, Exchange, ExchangeRewriter, MAX_EXCHANGE_BODY};
pub use foreign::ForeignCredentialWarning;
pub(crate) use guest::{ALPN_HTTP11, Prefixed};
pub use inject::{
    HeaderError, InjectContext, InjectDecision, InjectRefusal, InjectedHeader, Injection, Injector,
    NoInjection, RequestView, SecretValue, Unauthorized,
};
pub use intercept::{Interception, LEARNED_CAP, LEARNED_TTL, PassThrough};
pub use request_rules::{RequestRule, RequestRules};
pub(crate) use session::{Context, MAX_UNSWAPPED_STAND_INS, Outcome, run};
/// The HTTP/1.1 pieces the plain-HTTP path shares with a decrypted connection: the upstream
/// client, the guest's body streamed into it, and the response rebuilt for the guest.
pub(crate) mod h1 {
    pub(crate) use super::body::{Abort, ChannelBody};
    pub(crate) use super::exchange::AnswerBody;
    pub(crate) use super::leg::{
        BoxError, H1Conn, READY_WAIT, UpBody, h1 as handshake, setup_refusal,
    };
    pub(crate) use super::request::{Parsed, framed, upstream_headers};
    pub(crate) use super::response::{Written, write as write_response};
    pub(crate) use super::session::{keep_reading, send_and_pump, short};
}
pub use set::{PatternError, TerminationSet};
#[cfg(feature = "fuzzing")]
pub(crate) use stand_in::check_answer_scan;
pub use stand_in::{Placement, StandIn, StandInError, StandInOrigin, StandIns, secret_stand_in};
pub use token_body::{BodyField, BodyFormat, FieldKind, TokenBody, TokenBodyError};

/// Hosts Puddle decrypts by default: GitHub and Azure DevOps, for git and Git LFS. Not
/// `api.github.com` (the `gh` CLI and gists would become an exfiltration channel with a
/// credential), nor any package registry.
pub const DEFAULT_TERMINATED_HOSTS: [&str; 3] =
    ["github.com", "dev.azure.com", "*.visualstudio.com"];

/// What terminating one workspace's bound hosts needs: which hosts, the CA that certifies them for
/// that workspace alone, and the injector that decides about credentials.
#[derive(Debug)]
pub struct Termination {
    set: TerminationSet,
    ca: Arc<WorkspaceCa>,
    injector: Arc<dyn Injector>,
    stand_ins: Arc<StandIns>,
    exchanges: Option<Arc<dyn ExchangeRewriter>>,
    interception: Interception,
    request_rules: RequestRules,
    git_ssh_hosts: TerminationSet,
    git_ssh_keys: Option<Arc<crate::ssh_git::GitSshHostKeys>>,
    ssh_agent_hosts: HashMap<Host, AllowedKeys>,
    foreign_warning: Arc<ForeignCredentialWarning>,
}

/// How a connection to a host is decrypted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    /// The host has a credential, secret or capture profile: it is always decrypted, and a
    /// handshake or server that cannot be used fails like any other.
    Bound,
    /// The host is decrypted only because the workspace decrypts every host: whatever cannot be
    /// decrypted without breaking the tool is passed through instead.
    Everything,
}

impl Termination {
    /// Terminates the hosts of `set`, certified by `ca`, with credentials decided by `injector`.
    ///
    /// The CA has no name constraint, so `set` is the only limit on which hosts get a leaf from
    /// it; to change the set while a workspace runs, make a new `Termination` with the same CA
    /// and [`Terminations::insert`] it (connections already open keep the old set; the next one
    /// sees the new).
    #[must_use]
    pub fn new(set: TerminationSet, ca: Arc<WorkspaceCa>, injector: Arc<dyn Injector>) -> Self {
        Self {
            set,
            ca,
            injector,
            stand_ins: Arc::new(StandIns::new()),
            exchanges: None,
            interception: Interception::default(),
            request_rules: RequestRules::default(),
            git_ssh_hosts: TerminationSet::new(),
            git_ssh_keys: None,
            ssh_agent_hosts: HashMap::new(),
            foreign_warning: Arc::new(ForeignCredentialWarning::default()),
        }
    }

    /// Warns about a credential in a request that Puddle did not add or swap while `warning`
    /// says so (see [`ForeignCredentialWarning`]). On by default. Shared, so a new `Termination`
    /// for the same workspace keeps what the user was already told.
    #[must_use]
    pub fn with_foreign_credential_warning(
        mut self,
        warning: Arc<ForeignCredentialWarning>,
    ) -> Self {
        self.foreign_warning = warning;
        self
    }

    pub(crate) fn foreign_warning(&self) -> &Arc<ForeignCredentialWarning> {
        &self.foreign_warning
    }

    /// Sets how the workspace decrypts beyond its hosts with a credential (see [`Interception`]).
    #[must_use]
    pub fn with_interception(mut self, interception: Interception) -> Self {
        self.interception = interception;
        self
    }

    /// Refuses the requests `rules` name, and decrypts the hosts they name whatever the switch
    /// and the never-decrypt list say: a request that cannot be read cannot be judged.
    #[must_use]
    pub fn with_request_rules(mut self, rules: RequestRules) -> Self {
        self.set.extend(&rules.hosts());
        self.request_rules = rules;
        self
    }

    /// Also ends SSH to `hosts` itself (the hosts of the workspace's Git credentials: an
    /// enterprise server), on top of the Git hosts every workspace has (see
    /// [`crate::GitSsh`]). Not part of [`Self::set`]: a name there that has no Git credential
    /// (a login endpoint, a registry) is no Git host.
    #[must_use]
    pub fn with_git_ssh_hosts(mut self, hosts: TerminationSet) -> Self {
        self.git_ssh_hosts = hosts;
        self
    }

    /// Serves Git over SSH to this workspace with `keys`, the host keys of this start, instead of
    /// the keys the proxy's [`crate::GitSsh`] holds. They are the ones the guest's
    /// `ssh_known_hosts` lists.
    #[must_use]
    pub fn with_git_ssh_keys(mut self, keys: Arc<crate::ssh_git::GitSshHostKeys>) -> Self {
        self.git_ssh_keys = Some(keys);
        self
    }

    /// The keys of this start, when it has its own.
    #[must_use]
    pub fn git_ssh_keys(&self) -> Option<&crate::ssh_git::GitSshHostKeys> {
        self.git_ssh_keys.as_deref()
    }

    /// Lets the workspace use keys from the host's SSH agent on `hosts`, each with the keys it may
    /// use (see [`crate::ssh_agent`]). Empty by default: only the user turns the agent on.
    #[must_use]
    pub fn with_ssh_agent_hosts(mut self, hosts: HashMap<Host, AllowedKeys>) -> Self {
        self.ssh_agent_hosts = hosts;
        self
    }

    /// The hosts the filtered SSH agent is on for, with the keys each may use.
    #[must_use]
    pub fn ssh_agent_hosts(&self) -> &HashMap<Host, AllowedKeys> {
        &self.ssh_agent_hosts
    }

    /// The hosts whose SSH is ended for Git, beyond the built-in ones.
    #[must_use]
    pub fn git_ssh_hosts(&self) -> &TerminationSet {
        &self.git_ssh_hosts
    }

    /// The workspace's rules on method and path.
    #[must_use]
    pub fn request_rules(&self) -> &RequestRules {
        &self.request_rules
    }

    /// The workspace's switch, its never-decrypt list and the hosts learned so far.
    #[must_use]
    pub fn interception(&self) -> &Interception {
        &self.interception
    }

    /// How a connection to `host` is decrypted, or `None` when it is passed through.
    /// `from_container`: the agent says the client is in a container (see
    /// [`puddle_agent_proto::origin`]).
    pub(crate) fn mode_for(&self, host: &Host, from_container: bool) -> Option<Mode> {
        if self.set.contains(host) {
            Some(Mode::Bound)
        } else if self.interception.decrypt_all
            && matches!(host, Host::Name(_))
            && self.skipped(host, from_container).is_none()
        {
            Some(Mode::Everything)
        } else {
            None
        }
    }

    /// Why `host`, which the workspace would decrypt only because it decrypts every host, is
    /// passed through instead (the audit code): a container's client (it has none of the
    /// workspace's trust settings), the never-decrypt list, or a host its tools did not accept the
    /// certificate for. `None` for a host with a credential, which is always decrypted so the
    /// credential can be added, and for a host that is decrypted.
    pub(crate) fn skipped(&self, host: &Host, from_container: bool) -> Option<&'static str> {
        if self.set.contains(host) {
            return None;
        }
        if from_container && self.interception.decrypt_all && matches!(host, Host::Name(_)) {
            return Some("container");
        }
        self.interception.skipped(host)
    }

    /// Swaps the workspace's `stand_ins` for their real values on terminated requests (see
    /// [`StandIns`]). The registry stays the caller's to change while requests are served. Its
    /// hosts ([`StandIns::hosts`]) must be among the hosts this termination decrypts, or a
    /// connection to them is spliced and nothing is swapped.
    #[must_use]
    pub fn with_stand_ins(mut self, stand_ins: Arc<StandIns>) -> Self {
        self.stand_ins = stand_ins;
        self
    }

    /// Reads the token exchanges `exchanges` asks for (see [`ExchangeRewriter`]), on both HTTP
    /// versions. Its hosts must be among the hosts this termination decrypts.
    #[must_use]
    pub fn with_exchanges(mut self, exchanges: Arc<dyn ExchangeRewriter>) -> Self {
        self.exchanges = Some(exchanges);
        self
    }

    /// Whether `host` is a token endpoint of the workspace's logins (see
    /// [`ExchangeRewriter::is_token_endpoint`]); `false` without a rewriter.
    pub(crate) fn is_token_endpoint(&self, host: &Host) -> bool {
        self.exchanges
            .as_deref()
            .is_some_and(|exchanges| exchanges.is_token_endpoint(host))
    }

    /// The hosts that are decrypted.
    #[must_use]
    pub fn set(&self) -> &TerminationSet {
        &self.set
    }

    /// The CA that certifies every host of the set, for this workspace alone.
    #[must_use]
    pub fn ca(&self) -> &Arc<WorkspaceCa> {
        &self.ca
    }

    pub(crate) fn injector(&self) -> &Arc<dyn Injector> {
        &self.injector
    }

    /// The workspace's stand-ins, as [`Self::with_stand_ins`] was given them.
    #[must_use]
    pub fn stand_ins(&self) -> &Arc<StandIns> {
        &self.stand_ins
    }

    /// The reader of the workspace's token exchanges, as [`Self::with_exchanges`] was given it.
    #[must_use]
    pub fn exchanges(&self) -> Option<&dyn ExchangeRewriter> {
        self.exchanges.as_deref()
    }
}

/// Where the proxy finds a workspace's [`Termination`]. Asked once per `CONNECT`; `None` means the
/// workspace has no credential bound and everything is spliced.
pub trait TerminationSource: Send + Sync + std::fmt::Debug {
    /// The termination of `workspace`, if it has one.
    fn termination(&self, workspace: &WorkspaceId) -> Option<Arc<Termination>>;
}

/// A [`TerminationSource`] the host program updates as workspaces start, stop and change.
#[derive(Debug, Default)]
pub struct Terminations {
    workspaces: RwLock<HashMap<WorkspaceId, Arc<Termination>>>,
}

impl Terminations {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets (or replaces) `workspace`'s termination. Connections already open keep the old one.
    pub fn insert(&self, workspace: WorkspaceId, termination: Termination) {
        self.workspaces
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(workspace, Arc::new(termination));
    }

    /// Removes `workspace`'s termination (its CA is dropped once its connections end).
    pub fn remove(&self, workspace: &WorkspaceId) {
        self.workspaces
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(workspace);
    }
}

impl TerminationSource for Terminations {
    fn termination(&self, workspace: &WorkspaceId) -> Option<Arc<Termination>> {
        self.workspaces
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(workspace)
            .cloned()
    }
}
