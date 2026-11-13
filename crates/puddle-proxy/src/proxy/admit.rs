// SPDX-License-Identifier: GPL-3.0-or-later
//! Admission: the order of checks from a parsed request to a connected upstream.

use std::io;
use std::net::{IpAddr, SocketAddr};

use puddle_types::{
    BlockReason, ConnectionDecision, ConnectionEvent, ConnectionOutcome, ConnectionReason,
    Decision, EgressRequest, Host, PatternKind, PendingOutcome, PolicyError, RefusedReason, RuleId,
    SuffixAllows,
};

use puddle_netpolicy::{LocalCategory, block_message, company_network_message};

use crate::destination::AddressVerdict;
use crate::upstream::{Admitted, Upstream};

use super::{Proxy, Refusal};

/// Decides the request and, if allowed, returns the addresses it may connect to (R-10, R-14),
/// with the audit event for the decision.
pub(crate) async fn admit(
    proxy: &Proxy,
    request: &EgressRequest,
) -> (Result<Admitted, Refusal>, ConnectionEvent) {
    // Replaced by the first step that decides; a path that forgets to stays fail-closed in the
    // audit too.
    let mut event = ConnectionEvent::new(
        request,
        ConnectionDecision::Blocked,
        ConnectionReason::PolicyUnavailable,
    );
    let admitted = admit_into(proxy, request, &mut event).await;
    (admitted, event)
}

/// Records each decision `admit` takes in `event`.
fn note(
    event: &mut ConnectionEvent,
    request: &EgressRequest,
    decided: &Result<Decision, PolicyError>,
) {
    *event = match decided {
        Ok(decision) => ConnectionEvent::decided(request, decision),
        Err(_) => ConnectionEvent::new(
            request,
            ConnectionDecision::Blocked,
            ConnectionReason::PolicyUnavailable,
        ),
    };
}

/// Marks `event` blocked for `reason`, keeping the rule that allowed the name, if any.
pub(super) fn note_block(event: &mut ConnectionEvent, reason: BlockReason) {
    event.decision = ConnectionDecision::Blocked;
    event.reason = ConnectionReason::Blocked(reason);
    event.pending_id = None;
}

/// The addresses of the destination: an IP literal is its own, a name is resolved with the
/// proxy's timeout. Nothing resolved (or no answer in time) is the early end [`unresolved`]
/// gives, returned as the error: the destination is left to the company proxy or refused.
pub(super) async fn resolve_destination(
    proxy: &Proxy,
    host: &Host,
    port: u16,
) -> Result<Vec<SocketAddr>, Result<Admitted, Refusal>> {
    let addrs = match host {
        Host::Ip(ip) => vec![SocketAddr::new(*ip, port)],
        Host::Name(name) => {
            match tokio::time::timeout(
                proxy.config.resolve_timeout,
                proxy.resolver.resolve(name, port),
            )
            .await
            {
                Err(_) => {
                    return Err(unresolved(proxy, host, Unresolved::TimedOut));
                }
                Ok(Err(err)) => {
                    tracing::info!(%host, error = %err, "resolve failed");
                    return Err(unresolved(proxy, host, Unresolved::from_error(&err)));
                }
                Ok(Ok(addrs)) => addrs,
            }
        }
    };
    if addrs.is_empty() {
        return Err(unresolved(proxy, host, Unresolved::NoAddresses));
    }
    Ok(addrs)
}

async fn admit_into(
    proxy: &Proxy,
    request: &EgressRequest,
    event: &mut ConnectionEvent,
) -> Result<Admitted, Refusal> {
    let host = &request.host;
    let port = request.port;
    // SSH to a Git host's name is refused whatever the destination is: before the name stage (so
    // the reason is the protocol, not a toggle that turning on would only lead to the same
    // refusal) and before any rule, so it never becomes a pending row or an allow. SSH to any
    // other name is decided below like every connection.
    if proxy.refuses_ssh(request) {
        note_block(event, BlockReason::SshUnsupported);
        let mut refusal = block(request, &[BlockReason::SshUnsupported]);
        if let Some(message) = proxy.azure_devops_ssh_message(request).await {
            refusal.message = message;
        }
        return Err(refusal);
    }
    // Name stage: a literal or a name that is blocked by itself never reaches the rules, so it
    // never becomes a pending row (R-14).
    let target = puddle_netpolicy::Target::from_host(host.clone());
    if let Some(reason) = proxy
        .addresses
        .check_target(&request.workspace, &target, port)
    {
        note_block(event, reason);
        return Err(block(request, &[reason]));
    }
    let decided = proxy.decide(request, SuffixAllows::Count).await;
    note(event, request, &decided);
    let pattern = allowed(request, decided)?;
    let addrs = match resolve_destination(proxy, host, port).await {
        Ok(addrs) => addrs,
        Err(done) => return done,
    };
    let resolved = addrs.len();
    let (addrs, ip_allows) = apply_ip_rules(proxy, request, addrs, event)?;
    let mut usable = Vec::new();
    let mut exact_only = Vec::new();
    let mut categories: Vec<LocalCategory> = Vec::new();
    let mut blocked = Vec::new();
    let mut refused_first = None;
    let mut company_ip = None;
    for addr in addrs {
        if event.company_network.is_none() {
            event.company_network = proxy.addresses.company_signal(host, addr);
        }
        match proxy.addresses.check_named(&request.workspace, host, addr) {
            AddressVerdict::Allow => usable.push(addr),
            AddressVerdict::ExactOnly(category) => {
                exact_only.push(addr);
                categories.push(category);
            }
            AddressVerdict::Block(reason) => {
                if matches!(reason, BlockReason::CompanyNetwork(_)) {
                    company_ip.get_or_insert(addr.ip());
                }
                refused_first.get_or_insert((addr.ip(), RefusedReason::Guard(reason)));
                blocked.push(reason);
            }
            other => {
                tracing::warn!(%addr, verdict = ?other, "unknown address verdict, address dropped");
                blocked.push(BlockReason::LocalAddress);
            }
        }
    }
    event.refused_address = event.refused_address.or(refused_first);
    if pattern == PatternKind::Exact {
        // An exact allow reaches local addresses whose toggle is on.
        usable.append(&mut exact_only);
    } else if !exact_only.is_empty() {
        // After a wildcard allow, a local address whose toggle is on needs an exact allow of its
        // own (R-14): an exact rule for the address itself admits it.
        let (by_ip, rest): (Vec<_>, Vec<_>) = exact_only.into_iter().partition(|addr| {
            let rule = ip_allows.iter().find(|(allowed, _)| allowed == addr);
            if let Some((_, rule_id)) = rule {
                tracing::info!(workspace = %request.workspace, host = %request.host, %addr, rule = %rule_id, "local address allowed by its IP rule");
            }
            rule.is_some()
        });
        usable.extend(by_ip);
        exact_only = rest;
    }
    if usable.is_empty() && !exact_only.is_empty() {
        // Otherwise only an exact allow of the name counts. Asking again without suffix allows
        // writes the pending row for the exact name.
        let again = proxy.decide(request, SuffixAllows::Ignore).await;
        note(event, request, &again);
        if allowed(request, again).map_err(|r| wildcard_note(r, &categories))? != PatternKind::Exact
        {
            note_block(event, BlockReason::LocalAddress);
            return Err(block(request, &[BlockReason::LocalAddress]));
        }
        usable = exact_only;
    }
    if usable.is_empty() {
        if blocked.is_empty() {
            blocked.push(BlockReason::LocalAddress);
        }
        note_block(event, shown_reason(&blocked));
        return Err(block_at(request, &blocked, company_ip));
    }
    // A proxy may be told the name only when nothing the name resolved to was dropped: otherwise
    // it could pick the dropped address itself (R-14, R-27).
    let name_ok = matches!(host, Host::Ip(_)) || usable.len() == resolved;
    Ok(Admitted::checked(usable, name_ok))
}

/// How resolving a name here came to nothing.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Unresolved {
    TimedOut,
    /// The resolver answered that the name does not exist ([`io::ErrorKind::NotFound`]).
    NoSuchName,
    /// The lookup itself failed (no resolver answered, the network is down): the name may exist.
    Failed,
    NoAddresses,
}

impl Unresolved {
    /// What an error from [`Resolver::resolve`] means, by the `NotFound` contract.
    pub(crate) fn from_error(err: &io::Error) -> Self {
        if err.kind() == io::ErrorKind::NotFound {
            Self::NoSuchName
        } else {
            Self::Failed
        }
    }

    /// The reason for the log and the response the guest gets: a `504` for a timeout and a `502`
    /// for anything else, each saying which.
    pub(crate) fn refusal(self, host: &Host) -> (&'static str, Refusal) {
        let (why, status, message, outcome) = match self {
            Self::TimedOut => (
                "timed out",
                "504 Gateway Timeout",
                format!("resolving {host} timed out"),
                ConnectionOutcome::Timeout,
            ),
            Self::NoSuchName => (
                "no such name",
                "502 Bad Gateway",
                format!("no such name: {host} does not exist"),
                ConnectionOutcome::ConnectFailed,
            ),
            Self::Failed => (
                "failed",
                "502 Bad Gateway",
                format!("the lookup for {host} failed, so it is not known whether the name exists"),
                ConnectionOutcome::ConnectFailed,
            ),
            Self::NoAddresses => (
                "returned no addresses",
                "502 Bad Gateway",
                format!("{host} has no addresses"),
                ConnectionOutcome::ConnectFailed,
            ),
        };
        (why, Refusal::new(status, message).failed(outcome))
    }
}

/// A name this host could not resolve: the company proxy resolves it when that is on, else the
/// guest gets the refusal and the audit says which.
fn unresolved(proxy: &Proxy, host: &Host, how: Unresolved) -> Result<Admitted, Refusal> {
    let (why, refusal) = how.refusal(host);
    left_to_upstream(proxy, host, why).ok_or(refusal)
}

fn left_to_upstream(proxy: &Proxy, host: &Host, why: &str) -> Option<Admitted> {
    if proxy
        .upstream
        .as_ref()
        .is_some_and(Upstream::resolves_unknown_names)
    {
        tracing::info!(%host, why, "name not resolved here; the company proxy resolves it");
        Some(Admitted::unresolved())
    } else {
        None
    }
}

/// Applies each resolved address's own rules (R-27): drops every address an IP rule denies, and
/// returns the rest with the addresses an exact IP rule allows (for R-14). A literal was decided
/// as itself, so only a name's addresses are looked up.
fn apply_ip_rules(
    proxy: &Proxy,
    request: &EgressRequest,
    addrs: Vec<SocketAddr>,
    event: &mut ConnectionEvent,
) -> Result<(Vec<SocketAddr>, Vec<IpRuleHit>), Refusal> {
    let mut ip_allows = Vec::new();
    if matches!(request.host, Host::Ip(_)) {
        return Ok((addrs, ip_allows));
    }
    let mut ip_denies = Vec::new();
    let mut kept = Vec::new();
    for addr in addrs {
        match ip_rule(proxy, request, addr) {
            Ok(IpRule::Allow(rule_id)) => {
                ip_allows.push((addr, rule_id));
                kept.push(addr);
            }
            Ok(IpRule::Deny(rule_id)) => ip_denies.push((addr, rule_id)),
            Ok(IpRule::NoRule) => kept.push(addr),
            Err(err) => {
                tracing::warn!(%addr, error = %err, "IP rule lookup failed, refused");
                note(event, request, &Err(err));
                return Err(unavailable());
            }
        }
    }
    event.refused_address = ip_denies
        .first()
        .map(|(addr, _)| (addr.ip(), RefusedReason::IpRule));
    if kept.is_empty() {
        return Err(ip_denied(request, &ip_denies, event));
    }
    Ok((kept, ip_allows))
}

/// A resolved address and the IP rule that matched it.
type IpRuleHit = (SocketAddr, RuleId);

/// What the rules say about one resolved address by itself.
enum IpRule {
    /// No rule for the address.
    NoRule,
    /// An exact allow: admits a local address after a wildcard allow (R-14).
    Allow(RuleId),
    /// A deny: the address is never used (R-27).
    Deny(RuleId),
}

/// The rule that decides `addr`'s IP for `request`'s workspace. Looks only; a miss records nothing.
fn ip_rule(
    proxy: &Proxy,
    request: &EgressRequest,
    addr: SocketAddr,
) -> Result<IpRule, PolicyError> {
    let by_ip = EgressRequest::new(request.workspace.clone(), Host::Ip(addr.ip()), request.port);
    Ok(match proxy.policy.lookup(&by_ip, SuffixAllows::Ignore)? {
        Some(Decision::Allow {
            rule_id,
            pattern: PatternKind::Exact,
        }) => IpRule::Allow(rule_id),
        Some(
            Decision::Deny { rule_id, .. }
            | Decision::SetDeny {
                rule_id: Some(rule_id),
                ..
            },
        ) => IpRule::Deny(rule_id),
        _ => IpRule::NoRule,
    })
}

/// The refusal when every address of an allowed name is denied by its IP rule (R-27). It is a
/// deny, not pending: approving the name can't change it, so no pending row is written.
fn ip_denied(
    request: &EgressRequest,
    denies: &[IpRuleHit],
    event: &mut ConnectionEvent,
) -> Refusal {
    let host = &request.host;
    let list = denies
        .iter()
        .map(|(addr, rule_id)| format!("{} (rule {rule_id})", addr.ip()))
        .collect::<Vec<_>>()
        .join(", ");
    tracing::info!(workspace = %request.workspace, %host, port = request.port, addresses = %list, "every address denied by its IP rule");
    let first = denies.first();
    event.decision = ConnectionDecision::Deny;
    event.reason = ConnectionReason::Rule;
    event.rule_id = first.map(|(_, rule_id)| *rule_id);
    event.pending_id = None;
    event.resolved_ip = first.map(|(addr, _)| addr.ip());
    let refusal = Refusal::new(
        "403 Forbidden",
        format!("{host} resolves only to addresses denied by a rule: {list}"),
    )
    .header("x-puddle-decision", "deny");
    match first {
        Some((_, rule_id)) => refusal.header("x-puddle-rule", rule_id.to_string()),
        None => refusal,
    }
}

/// The refusal when the rules could not be checked.
fn unavailable() -> Refusal {
    Refusal::new(
        "503 Service Unavailable",
        "the rules could not be checked; the connection was refused",
    )
}

/// Adds to a pending refusal why the wildcard allow didn't count.
fn wildcard_note(mut refusal: Refusal, categories: &[LocalCategory]) -> Refusal {
    if let Some(category) = categories.first() {
        refusal.message = format!(
            "{} (it resolves to a {} address, and wildcard rules don't reach local addresses: approve the exact name, add an exact rule for its address, or turn on \"wildcards reach local addresses\")",
            refusal.message,
            category.describe()
        );
    }
    refusal
}

/// The pattern kind of an allow, or the refusal for anything else.
fn allowed(
    request: &EgressRequest,
    decision: Result<Decision, PolicyError>,
) -> Result<PatternKind, Refusal> {
    let host = &request.host;
    let port = request.port;
    let workspace = &request.workspace;
    match decision {
        Ok(Decision::Allow { rule_id, pattern }) => {
            tracing::info!(%workspace, %host, port, rule = %rule_id, "allowed");
            Ok(pattern)
        }
        Ok(Decision::Deny { rule_id, .. }) => {
            tracing::info!(%workspace, %host, port, rule = %rule_id, "denied by rule");
            Err(Refusal::new(
                "403 Forbidden",
                format!("{host} is denied by a rule (rule {rule_id})"),
            )
            .header("x-puddle-decision", "deny")
            .header("x-puddle-rule", rule_id.to_string()))
        }
        // A set's allow counts like a wildcard allow for local destinations (R-42).
        Ok(Decision::SetAllow { set, .. }) => {
            tracing::info!(%workspace, %host, port, %set, "allowed by a rule set");
            Ok(PatternKind::Suffix)
        }
        Ok(Decision::SetDeny { set, rule_id, .. }) => {
            tracing::info!(%workspace, %host, port, %set, rule = ?rule_id, "denied by a rule set");
            let refusal = match rule_id {
                Some(rule_id) => Refusal::new(
                    "403 Forbidden",
                    format!(
                        "{host} is denied by rule {rule_id} of your rule set {set} (listed under Rules, Rule sets); delete that entry, switch the set off for this workspace, or add an exact allow of your own"
                    ),
                )
                .header("x-puddle-rule", rule_id.to_string()),
                None => Refusal::new(
                    "403 Forbidden",
                    format!(
                        "{host} is on the built-in rule set {set} (listed under Rules, Rule sets), which blocks it; switch the set off for this workspace, or add an exact allow of your own"
                    ),
                ),
            };
            Err(refusal
                .header("x-puddle-decision", "deny")
                .header("x-puddle-rule-set", set.to_string()))
        }
        Ok(Decision::Pending(outcome)) => {
            tracing::info!(%workspace, %host, port, pending = ?outcome.pending_id(), "pending approval");
            let refusal = Refusal::new(
                "403 Forbidden",
                match outcome {
                    PendingOutcome::Unlisted => format!(
                        "{host} is not listed for approval in Puddle yet: this workspace already has more waiting requests than Puddle lists. Answer some of them in Puddle, then retry and this one is listed"
                    ),
                    _ => format!(
                        "{host} is waiting for approval in Puddle; approve it there, then retry"
                    ),
                },
            )
            .header("x-puddle-decision", "pending");
            Err(match outcome.pending_id() {
                Some(id) => refusal.header("x-puddle-pending", id.to_string()),
                None => refusal,
            })
        }
        Ok(Decision::Blocked { reason }) => Err(block(request, &[reason])),
        Ok(other) => {
            tracing::warn!(%workspace, %host, port, decision = ?other, "unknown decision, refused");
            Err(
                Refusal::new("403 Forbidden", format!("{host} is not allowed"))
                    .header("x-puddle-decision", "deny"),
            )
        }
        Err(err) => {
            tracing::error!(%workspace, %host, port, error = %err, "policy unavailable, refused");
            Err(unavailable())
        }
    }
}

/// The refusal for a blocked request; `reasons` holds every address's reason (at least one).
/// The header names the first toggle if any (turning it on would help), else the first reason.
fn block(request: &EgressRequest, reasons: &[BlockReason]) -> Refusal {
    block_at(request, reasons, None)
}

/// [`block`] for a block at an address of the destination: a company-network refusal names it.
fn block_at(request: &EgressRequest, reasons: &[BlockReason], ip: Option<IpAddr>) -> Refusal {
    let host = &request.host;
    let reason = shown_reason(reasons);
    tracing::info!(workspace = %request.workspace, %host, port = request.port, %reason, "blocked");
    let message = match reason {
        BlockReason::CompanyNetwork(signal) => company_network_message(host, ip, Some(signal)),
        _ => block_message(host, &request.workspace, reasons),
    };
    Refusal::new("403 Forbidden", message)
        .header("x-puddle-decision", "blocked")
        .header("x-puddle-blocked", reason.code())
}

/// The reason a block names: the first toggle or unreadable-settings reason if any (turning the
/// toggle on or fixing the settings would help), else the first.
fn shown_reason(reasons: &[BlockReason]) -> BlockReason {
    reasons
        .iter()
        .find(|r| {
            matches!(
                r,
                BlockReason::LocalToggle(_)
                    | BlockReason::CompanyNetwork(_)
                    | BlockReason::SettingsUnreadable
            )
        })
        .or_else(|| reasons.first())
        .copied()
        .unwrap_or(BlockReason::LocalAddress)
}
