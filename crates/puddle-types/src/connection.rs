// SPDX-License-Identifier: AGPL-3.0-or-later
//! One connection as the proxy reports it for the audit (`docs/spec/rules.md` R-24).
//!
//! The proxy builds a [`ConnectionEvent`] and hands it to a [`ConnectionLog`]; the store
//! implements the log and turns the event into a `connection` record. Neither crate depends
//! on the other. Nothing secret is representable: there are no header, body or credential
//! fields, and an [`HttpRequestLine`] keeps only the path of a request target (R-25).

use std::fmt;
use std::net::IpAddr;

use serde::{Deserialize, Serialize};

use crate::{
    BlockReason, CompanySignal, Decision, EgressRequest, Host, PendingId, RuleId, RuleSetId,
    WorkspaceId,
};

/// How the proxy handled a connection (R-24).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ConnectionDecision {
    /// Let through.
    Allow,
    /// Refused by a deny rule.
    Deny,
    /// Refused while waiting for the user.
    Pending,
    /// Refused regardless of rules (toggle off, Puddle's own endpoint, unsupported protocol,
    /// rules unavailable).
    Blocked,
}

/// Who the connection belongs to (R-24): a workspace's traffic, or Puddle's own (image pulls
/// through the pull proxy, which have no workspace). Records written before this existed read as
/// [`ConnectionOrigin::Workspace`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ConnectionOrigin {
    /// A workspace's connection. Older records name it `sandbox`.
    #[default]
    #[serde(alias = "sandbox")]
    Workspace,
    /// Puddle's own connection, made on the host and not for any workspace.
    Puddle,
}

impl ConnectionOrigin {
    /// The stored and wire name: `workspace` or `puddle`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Workspace => "workspace",
            Self::Puddle => "puddle",
        }
    }
}

impl fmt::Display for ConnectionOrigin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How reaching the destination went, once the proxy had decided to let a connection through
/// (R-24). A refusal Puddle made on purpose has none: a `403` is a decision, not an outcome.
/// Without this an allowed connection that ended in a `502` or `504` reads like any other allow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ConnectionOutcome {
    /// The connection to the destination (directly, or through the company proxy) was made.
    Connected,
    /// The company proxy refused the destination, or the sign-in to it failed or is not possible
    /// (the guest got a `403` or `502`).
    UpstreamRefused,
    /// No route to the destination worked: it did not resolve, every hop was unreachable, or the
    /// destination refused the connection (a `502`).
    ConnectFailed,
    /// Resolving or connecting took too long (a `504`, or a hop that never answered).
    Timeout,
}

impl ConnectionOutcome {
    /// The stored and wire name.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Connected => "connected",
            Self::UpstreamRefused => "upstream_refused",
            Self::ConnectFailed => "connect_failed",
            Self::Timeout => "timeout",
        }
    }
}

impl fmt::Display for ConnectionOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why one address of an answer was refused ([`ConnectionEvent::refused_address`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RefusedReason {
    /// The address guard refused it.
    Guard(BlockReason),
    /// An IP rule denies the address (R-27).
    IpRule,
}

impl RefusedReason {
    /// The audit `refused_reason` code.
    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            Self::Guard(reason) => reason.code(),
            Self::IpRule => "ip_rule",
        }
    }
}

impl From<BlockReason> for RefusedReason {
    fn from(reason: BlockReason) -> Self {
        Self::Guard(reason)
    }
}

/// Why the proxy decided as it did (R-24). Serialised as a string: `rule`, `no_rule`, a
/// [`BlockReason::code`] (`toggle:<category>`, `puddle_endpoint`, `ssh_unsupported`, `udp_unsupported`,
/// `local_address`, `settings_unreadable`), `policy_unavailable`, `puddle_request`, `pki_distribution_point`, `sni_mismatch`, `guest_tls_rejected`, `certificate_unavailable`,
/// `connection_limit`, `request_rule`, `suppressed`, `merged`, `limit:<code>` (a limit ended the connection or cut a stream of it), `pass_through:<code>` (a host the workspace decrypts only because it decrypts every host was passed through untouched), `puddle_request_failed:<code>` or, for a request the credential rules refused on a decrypted connection, the
/// refusal's code (`push_denied`, `pull_denied`, `credential_unavailable`, `token_endpoint_spliced`, ...).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ConnectionReason {
    /// A rule decided.
    Rule,
    /// No rule matched.
    NoRule,
    /// Blocked whatever the rules say.
    Blocked(BlockReason),
    /// The rules could not be checked; the proxy failed closed.
    PolicyUnavailable,
    /// Puddle's own request, which no rule decides (an image pull that passed the address guard).
    PuddleRequest,
    /// A plain-HTTP `GET` or `HEAD` of a certificate distribution point or authority-information-
    /// access URL named in a root the host synced, to an address on the company or private
    /// network: allowed without a rule and without the company-network toggle.
    PkiDistributionPoint,
    /// A rule on method and path refused a request on a decrypted connection (R-49); the
    /// record's `request_rule` names it and `method` and `path` the request.
    RequestRule,
    /// A terminated TLS connection's client asked for a name other than the one it
    /// `CONNECT`ed to (or none): the handshake was refused and nothing went upstream.
    SniMismatch,
    /// A terminated TLS connection's client refused Puddle's certificate for the host (it does not
    /// trust the workspace's CA). Nothing is blocked: the decision of the rule stands.
    GuestTlsRejected,
    /// A terminated TLS connection's handshake failed because Puddle could not make the
    /// certificate for the host (its CA failed to issue it): Puddle's own failure, not the
    /// client's. Nothing is blocked on purpose: the decision of the rule stands.
    CertificateUnavailable,
    /// The workspace already had its limit of open connections, so this one was refused with a
    /// `503` before the rules were asked.
    ConnectionLimit,
    /// Summary of connection records over the per-workspace limit (R-26).
    Suppressed,
    /// Summary of the oldest `connection` records of waiting requests, merged into one record
    /// to keep a flood of them from filling the audit (R-26).
    Merged,
    /// On a decrypted connection the credential rules refused a request, or the server's `401`
    /// to a request whose credential Puddle chose was answered with Puddle's own refusal. The
    /// text is the refusal's machine-readable code.
    Refused(&'static str),
    /// A limit of the proxy ended the connection, or cut a stream or pipe of it (a stream idle
    /// for an hour, an agreed WebSocket that went silent, a flood of control frames, a header
    /// block left open). The decision of the rule stands. The text is the limit's code, shown as
    /// `limit:<code>`. A connection records the first limit that fired.
    LimitEnded(&'static str),
    /// A host the workspace decrypts only because it decrypts every host was passed through to
    /// the real server untouched: it is on the never-decrypt list (`never_decrypt`), a tool did
    /// not accept Puddle's certificate for it before (`learned`), the client did not start a
    /// TLS handshake for the name (`not_tls`, `sni_mismatch`), or the server cannot be talked
    /// to through Puddle's TLS (`server_client_certificate`, `server_certificate`, `server_tls`).
    /// The decision of the rule stands. Shown as `pass_through:<code>`.
    PassedThrough(&'static str),
    /// Puddle's own request (a repository listing) that could not be completed: the code says
    /// why (`unreachable`, `tls`, `timeout`, `too_large`, `protocol`, `bad_token`, or
    /// `host_refused` for a 401 or 403, `rate_limited` for a 429, `host_error` for any other error
    /// status).
    PuddleRequestFailed(&'static str),
}

impl fmt::Display for ConnectionReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Rule => f.write_str("rule"),
            Self::NoRule => f.write_str("no_rule"),
            Self::Blocked(reason) => f.write_str(reason.code()),
            Self::PolicyUnavailable => f.write_str("policy_unavailable"),
            Self::PuddleRequest => f.write_str("puddle_request"),
            Self::PkiDistributionPoint => f.write_str("pki_distribution_point"),
            Self::RequestRule => f.write_str("request_rule"),
            Self::SniMismatch => f.write_str("sni_mismatch"),
            Self::ConnectionLimit => f.write_str("connection_limit"),
            Self::GuestTlsRejected => f.write_str("guest_tls_rejected"),
            Self::CertificateUnavailable => f.write_str("certificate_unavailable"),
            Self::Suppressed => f.write_str("suppressed"),
            Self::Merged => f.write_str("merged"),
            Self::LimitEnded(code) => write!(f, "limit:{code}"),
            Self::Refused(code) => f.write_str(code),
            Self::PassedThrough(code) => write!(f, "pass_through:{code}"),
            Self::PuddleRequestFailed(code) => write!(f, "puddle_request_failed:{code}"),
        }
    }
}

/// The path of a request target, without scheme, authority (and userinfo), query string or
/// fragment (R-25).
///
/// ```
/// use puddle_types::request_path;
/// assert_eq!(request_path("/a/b?token=x#f"), "/a/b");
/// assert_eq!(request_path("http://user:pw@h.example/x?q"), "/x");
/// assert_eq!(request_path("http://h.example"), "/");
/// ```
#[must_use]
pub fn request_path(target: &str) -> &str {
    let end = target.find(['?', '#']).unwrap_or(target.len());
    let target = target.get(..end).unwrap_or_default();
    // Only an absolute-form target (`http://host/path`) has an authority to cut off: a `://` later
    // in an origin-form path (`/a://b/c`) is part of the path.
    match target.find("://").filter(|_| !target.starts_with('/')) {
        Some(scheme_end) => {
            let rest = target.get(scheme_end + 3..).unwrap_or_default();
            rest.find('/')
                .and_then(|slash| rest.get(slash..))
                .unwrap_or("/")
        }
        None => target,
    }
}

/// The method and path of a request the proxy saw in clear. Only [`request_path`] of the target
/// is kept, so a query string, fragment or userinfo never gets in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpRequestLine {
    method: String,
    path: String,
}

impl HttpRequestLine {
    /// `target` as received (origin or absolute form); only its path is kept.
    #[must_use]
    pub fn new(method: impl Into<String>, target: &str) -> Self {
        Self {
            method: method.into(),
            path: request_path(target).to_owned(),
        }
    }

    /// `GET`, `POST`, ...
    #[must_use]
    pub fn method(&self) -> &str {
        &self.method
    }

    /// The path, without query string or fragment.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }
}

/// One connection as the proxy reports it; the store stamps the time.
///
/// Non-exhaustive: build it with [`ConnectionEvent::new`] or [`ConnectionEvent::decided`] and
/// set the remaining fields.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ConnectionEvent {
    /// Whose connection it is.
    pub origin: ConnectionOrigin,
    /// The workspace it came from (the route's); `None` for Puddle's own connections.
    pub workspace: Option<WorkspaceId>,
    /// The requested host.
    pub host: Host,
    /// The requested port.
    pub port: u16,
    /// The address connected to, once connected.
    pub resolved_ip: Option<IpAddr>,
    /// The first address of the answer that an IP rule denied, else the first the address guard
    /// refused, and why (its code is `ip_rule`, `toggle:private`, `local_address`, ...).
    /// Visibility only: the connection still goes to the allowed addresses of the answer alone.
    pub refused_address: Option<(IpAddr, RefusedReason)>,
    /// The signal that put the destination in the company-network category: `Some` only when an
    /// address of the answer was in it, whether that address was refused or allowed.
    pub company_network: Option<CompanySignal>,
    /// The hop of the company-proxy route that carried the connection (`DIRECT` or
    /// `PROXY host:port`), once connected; `None` when no upstream route is configured.
    /// Never carries credentials.
    pub upstream: Option<String>,
    /// What happened.
    pub decision: ConnectionDecision,
    /// How reaching the destination went, once it was allowed; `None` while nothing was
    /// attempted (a refusal by rule, guard or limit) and where the proxy cannot tell.
    pub outcome: Option<ConnectionOutcome>,
    /// Why.
    pub reason: ConnectionReason,
    /// The deciding rule.
    pub rule_id: Option<RuleId>,
    /// The rule set whose entry decided, if one did (R-24).
    pub rule_set: Option<RuleSetId>,
    /// The rule on method and path that refused a request of the connection, or for a tunnel
    /// refused because its host has such rules the lowest-numbered of them (R-24, R-49, R-54).
    /// Its own numbering, apart from [`Self::rule_id`].
    pub request_rule: Option<RuleId>,
    /// The pending row, when pending.
    pub pending_id: Option<PendingId>,
    /// The credential binding used, by id only. Never the credential.
    pub binding_id: Option<String>,
    /// Whether a credential was injected.
    pub injected: bool,
    /// The ids of the secret stand-ins that went out unchanged, each once: to a host outside the
    /// hosts they are for, or in a place they are not swapped in (a header secret in the path or
    /// query, a path or query secret in a header or another parameter). A stand-in is worthless
    /// there; the list shows a wrong host list or place, or a workspace that sends its
    /// environment somewhere it should not.
    /// An id reads `stand-in:<secret|login>:<name>`; it never carries a value.
    pub unswapped_stand_ins: Vec<String>,
    /// The ids of the stand-ins whose real values an answer from one of their hosts held, put back as
    /// the stand-in before the workspace saw it (`stand-in:secret:<NAME>` or `stand-in:login:<name>`),
    /// each once, at most 16.
    pub scrubbed_stand_ins: Vec<String>,
    /// What kind of credential the requests carried that Puddle did not add or swap, each kind
    /// once (`github_token`, `stale_stand_in`, ...; see [`crate::ForeignCredentialKind`]). Empty
    /// when none did and when the workspace turned the warning off. It never carries a value.
    pub foreign_credentials: Vec<String>,
    /// What kind of token the answers held that Puddle does not hold, each kind once
    /// (`github_token`, `jwt_with_refresh_token`, ...; see [`crate::UnknownTokenKind`]). Empty
    /// when none did and when the workspace turned the warning off. It never carries a value.
    pub answer_credentials: Vec<String>,
    /// Method and path, when the proxy saw the request in clear.
    pub http: Option<HttpRequestLine>,
    /// Bytes from the guest.
    pub bytes_up: u64,
    /// Bytes to the guest.
    pub bytes_down: u64,
}

impl ConnectionEvent {
    /// An event for `request` with no rule, row, credential, request line or bytes yet.
    #[must_use]
    pub fn new(
        request: &EgressRequest,
        decision: ConnectionDecision,
        reason: ConnectionReason,
    ) -> Self {
        Self {
            origin: ConnectionOrigin::Workspace,
            workspace: Some(request.workspace.clone()),
            host: request.host.clone(),
            port: request.port,
            resolved_ip: None,
            refused_address: None,
            company_network: None,
            upstream: None,
            decision,
            outcome: None,
            reason,
            rule_id: None,
            rule_set: None,
            request_rule: None,
            pending_id: None,
            binding_id: None,
            injected: false,
            unswapped_stand_ins: Vec::new(),
            scrubbed_stand_ins: Vec::new(),
            foreign_credentials: Vec::new(),
            answer_credentials: Vec::new(),
            http: None,
            bytes_up: 0,
            bytes_down: 0,
        }
    }

    /// An event for a connection Puddle makes itself (no workspace, origin
    /// [`ConnectionOrigin::Puddle`]), with no row, credential, request line or bytes.
    #[must_use]
    pub fn puddle(
        host: Host,
        port: u16,
        decision: ConnectionDecision,
        reason: ConnectionReason,
    ) -> Self {
        Self {
            origin: ConnectionOrigin::Puddle,
            workspace: None,
            host,
            port,
            resolved_ip: None,
            refused_address: None,
            company_network: None,
            upstream: None,
            decision,
            outcome: None,
            reason,
            rule_id: None,
            rule_set: None,
            request_rule: None,
            pending_id: None,
            binding_id: None,
            injected: false,
            unswapped_stand_ins: Vec::new(),
            scrubbed_stand_ins: Vec::new(),
            foreign_credentials: Vec::new(),
            answer_credentials: Vec::new(),
            http: None,
            bytes_up: 0,
            bytes_down: 0,
        }
    }

    /// The event for the engine's `decision` on `request`: decision, reason, rule and pending
    /// row filled in.
    ///
    /// ```
    /// use puddle_types::*;
    /// let request = EgressRequest::new(
    ///     WorkspaceId::new("box").unwrap(),
    ///     Host::parse_normalised("example.com").unwrap(),
    ///     443,
    /// );
    /// let event = ConnectionEvent::decided(
    ///     &request,
    ///     &Decision::Pending(PendingOutcome::New(PendingId(7))),
    /// );
    /// assert_eq!(event.decision, ConnectionDecision::Pending);
    /// assert_eq!(event.reason, ConnectionReason::NoRule);
    /// assert_eq!(event.pending_id, Some(PendingId(7)));
    /// ```
    #[must_use]
    pub fn decided(request: &EgressRequest, decision: &Decision) -> Self {
        let (kind, reason, rule_id, pending_id) = match *decision {
            Decision::SetAllow { rule_id, .. } => (
                ConnectionDecision::Allow,
                ConnectionReason::Rule,
                rule_id,
                None,
            ),

            Decision::Allow { rule_id, .. } => (
                ConnectionDecision::Allow,
                ConnectionReason::Rule,
                Some(rule_id),
                None,
            ),
            Decision::Deny { rule_id, .. } => (
                ConnectionDecision::Deny,
                ConnectionReason::Rule,
                Some(rule_id),
                None,
            ),
            Decision::SetDeny { rule_id, .. } => (
                ConnectionDecision::Deny,
                ConnectionReason::Rule,
                rule_id,
                None,
            ),
            Decision::Pending(outcome) => (
                ConnectionDecision::Pending,
                ConnectionReason::NoRule,
                None,
                outcome.pending_id(),
            ),
            Decision::Blocked { reason } => (
                ConnectionDecision::Blocked,
                ConnectionReason::Blocked(reason),
                None,
                None,
            ),
        };
        let mut event = Self::new(request, kind, reason);
        event.rule_id = rule_id;
        event.rule_set = decision.rule_set();
        event.pending_id = pending_id;
        event
    }
}

/// Where the proxy reports connections. Implemented by the store (`connection` audit records),
/// used by the proxy; the host program wires them.
///
/// Calls may write to the database, so async callers run them on a blocking thread. A log that
/// fails reports it itself (the proxy has no one to tell); the connection is not affected.
pub trait ConnectionLog: Send + Sync {
    /// Records `event`.
    fn record(&self, event: &ConnectionEvent);
}

/// A [`ConnectionLog`] that drops every event: for tests and tools without an audit.
#[derive(Debug, Clone, Copy, Default)]
pub struct NullConnectionLog;

impl ConnectionLog for NullConnectionLog {
    fn record(&self, _event: &ConnectionEvent) {}
}

impl<T: ConnectionLog + ?Sized> ConnectionLog for std::sync::Arc<T> {
    fn record(&self, event: &ConnectionEvent) {
        (**self).record(event);
    }
}

#[cfg(test)]
mod tests;
