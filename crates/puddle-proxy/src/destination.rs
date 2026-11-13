// SPDX-License-Identifier: GPL-3.0-or-later
//! Which destinations and resolved addresses the proxy may connect to (R-14), and how names are
//! resolved.
//!
//! The proxy resolves a name only after a rule allowed it (R-10), checks every
//! address it got, and connects only to an address that passed: it never resolves twice, so a
//! DNS answer can't change between the check and the connect. The checks are
//! `puddle-netpolicy`'s [`NetPolicy`]: address classes, the workspace's local-destination toggles
//! and Puddle's own endpoints.

use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;

use dns_lookup::{AddrInfoHints, LookupError, LookupErrorKind, SockType};

pub use puddle_netpolicy::AddressVerdict;
use puddle_netpolicy::{NetPolicy, Target};
use puddle_types::{BlockReason, CompanySignal, DomainName, Host, WorkspaceId};

/// Decides per destination and per resolved address (R-14). [`NetPolicy`] is the product
/// implementation; tests use doubles.
pub trait AddressCheck: Send + Sync {
    /// The name stage, before the rules are asked: `Some(reason)` blocks the request without a
    /// pending row. The default checks an IP literal with [`Self::check`] and lets names through.
    fn check_target(
        &self,
        workspace: &WorkspaceId,
        target: &Target,
        port: u16,
    ) -> Option<BlockReason> {
        match target.host() {
            Host::Ip(ip) => match self.check(workspace, SocketAddr::new(*ip, port)) {
                AddressVerdict::Block(reason) => Some(reason),
                _ => None,
            },
            Host::Name(_) => None,
        }
    }

    /// The verdict for connecting `workspace` to `addr`.
    fn check(&self, workspace: &WorkspaceId, addr: SocketAddr) -> AddressVerdict;

    /// The verdict for connecting `workspace` to `addr`, an address of the name `host`, which
    /// the company-network signals about the name (its DNS suffix, the proxy script) need. The
    /// default ignores the name.
    fn check_named(
        &self,
        workspace: &WorkspaceId,
        _host: &Host,
        addr: SocketAddr,
    ) -> AddressVerdict {
        self.check(workspace, addr)
    }

    /// The company-network signal that puts `addr` (an address of `host`) in that category, for
    /// the audit; `None` when it is not in it, and by default.
    fn company_signal(&self, _host: &Host, _addr: SocketAddr) -> Option<CompanySignal> {
        None
    }

    /// Whether `addr`, an address of `host`, is one a certificate distribution point may be
    /// fetched from without a rule: on the company or a private network. By default none
    /// is, so the exception only applies where the check says so.
    fn pki_address(&self, _host: &Host, _addr: SocketAddr) -> bool {
        false
    }
}

impl AddressCheck for NetPolicy {
    fn check_target(
        &self,
        workspace: &WorkspaceId,
        target: &Target,
        port: u16,
    ) -> Option<BlockReason> {
        NetPolicy::check_target(self, workspace, target, port)
    }

    fn check(&self, workspace: &WorkspaceId, addr: SocketAddr) -> AddressVerdict {
        self.check_address(workspace, addr)
    }

    fn check_named(
        &self,
        workspace: &WorkspaceId,
        host: &Host,
        addr: SocketAddr,
    ) -> AddressVerdict {
        self.check_address_named(workspace, Some(host), addr)
    }

    fn company_signal(&self, host: &Host, addr: SocketAddr) -> Option<CompanySignal> {
        NetPolicy::company_signal(self, Some(host), addr)
    }

    fn pki_address(&self, host: &Host, addr: SocketAddr) -> bool {
        NetPolicy::pki_address(self, Some(host), addr)
    }
}

/// A boxed future, for the object-safe [`Resolver`].
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Resolves a name the rules allowed. Tests fake it; [`SystemResolver`] uses the OS resolver.
pub trait Resolver: Send + Sync {
    /// The addresses of `name`, with `port` filled in.
    ///
    /// # Errors
    /// [`io::ErrorKind::NotFound`] when the name does not exist (NXDOMAIN, or no address of the
    /// asked kind); any other error means the lookup itself failed (no resolver answered, the
    /// network is down), which says nothing about the name.
    fn resolve<'a>(
        &'a self,
        name: &'a DomainName,
        port: u16,
    ) -> BoxFuture<'a, io::Result<Vec<SocketAddr>>>;
}

/// The OS resolver (`getaddrinfo` on a blocking thread, through tokio). A name under `.invalid`
/// (and the bare `invalid`) is answered "no such name" at once without asking the OS: RFC 6761
/// section 6.4 reserves it as never existing, and lets software say so itself. A private DNS that
/// would resolve such a name is not asked either.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemResolver;

impl Resolver for SystemResolver {
    fn resolve<'a>(
        &'a self,
        name: &'a DomainName,
        port: u16,
    ) -> BoxFuture<'a, io::Result<Vec<SocketAddr>>> {
        Box::pin(async move {
            if is_invalid_name(name.as_str()) {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    "the name is under .invalid, which never exists (RFC 6761)",
                ));
            }
            let host = name.as_str().to_owned();
            tokio::task::spawn_blocking(move || lookup(&host, port))
                .await
                .map_err(io::Error::other)?
        })
    }
}

/// Whether `host` is `invalid` or ends in `.invalid`, the label compared whole (`notinvalid.com`
/// and `invalid.example.com` are ordinary names) and ignoring case and a trailing root dot.
fn is_invalid_name(host: &str) -> bool {
    let host = host.strip_suffix('.').unwrap_or(host);
    let last = host.rsplit('.').next().unwrap_or(host);
    last.eq_ignore_ascii_case("invalid")
}

/// One blocking `getaddrinfo` for stream sockets, like `std`'s own lookup but with the error
/// code kept.
fn lookup(host: &str, port: u16) -> io::Result<Vec<SocketAddr>> {
    let hints = AddrInfoHints {
        socktype: SockType::Stream.into(),
        ..AddrInfoHints::default()
    };
    let infos = dns_lookup::getaddrinfo(Some(host), None, Some(hints)).map_err(name_error)?;
    let mut addrs = Vec::new();
    for info in infos {
        let mut addr = info?.sockaddr;
        addr.set_port(port);
        addrs.push(addr);
    }
    Ok(addrs)
}

/// `err` from `getaddrinfo`, with "the name does not exist" told apart from "the lookup failed"
/// by the error code, which is the same on every system: the first becomes
/// [`io::ErrorKind::NotFound`] (the original text kept), the second is passed on. A code that is
/// not known to mean "no such name" counts as a failure, so a name that exists is never reported
/// as missing.
fn name_error(err: LookupError) -> io::Error {
    if names_nothing(err.kind()) {
        io::Error::new(io::ErrorKind::NotFound, io::Error::from(err))
    } else {
        io::Error::from(err)
    }
}

/// Whether `kind` says the resolver answered that the name has no address (`EAI_NONAME`,
/// `EAI_NODATA`; `WSAHOST_NOT_FOUND`, `WSANO_DATA` on Windows) rather than that it could not
/// answer (`EAI_AGAIN`, `EAI_FAIL`, `EAI_SYSTEM`, memory, an unknown code).
const fn names_nothing(kind: LookupErrorKind) -> bool {
    matches!(kind, LookupErrorKind::NoName | LookupErrorKind::NoData)
}

#[cfg(test)]
mod tests;
