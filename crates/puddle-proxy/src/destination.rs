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
use puddle_types::{BlockReason, DomainName, Host, WorkspaceName};

/// Decides per destination and per resolved address (R-14). [`NetPolicy`] is the product
/// implementation; tests use doubles.
pub trait AddressCheck: Send + Sync {
    /// The name stage, before the rules are asked: `Some(reason)` blocks the request without a
    /// pending row. The default checks an IP literal with [`Self::check`] and lets names through.
    fn check_target(
        &self,
        workspace: &WorkspaceName,
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
    fn check(&self, workspace: &WorkspaceName, addr: SocketAddr) -> AddressVerdict;
}

impl AddressCheck for NetPolicy {
    fn check_target(
        &self,
        workspace: &WorkspaceName,
        target: &Target,
        port: u16,
    ) -> Option<BlockReason> {
        NetPolicy::check_target(self, workspace, target, port)
    }

    fn check(&self, workspace: &WorkspaceName, addr: SocketAddr) -> AddressVerdict {
        self.check_address(workspace, addr)
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

/// The OS resolver (`getaddrinfo` on a blocking thread, through tokio).
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemResolver;

impl Resolver for SystemResolver {
    fn resolve<'a>(
        &'a self,
        name: &'a DomainName,
        port: u16,
    ) -> BoxFuture<'a, io::Result<Vec<SocketAddr>>> {
        Box::pin(async move {
            let host = name.as_str().to_owned();
            tokio::task::spawn_blocking(move || lookup(&host, port))
                .await
                .map_err(io::Error::other)?
        })
    }
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
mod tests {
    use super::*;
    #[cfg(windows)]
    use windows_sys::Win32::Networking::WinSock::{
        WSA_NOT_ENOUGH_MEMORY, WSAENETDOWN, WSAHOST_NOT_FOUND, WSANO_DATA, WSANO_RECOVERY,
        WSATRY_AGAIN,
    };

    #[test]
    fn a_name_that_does_not_exist_is_told_apart_from_a_lookup_that_failed() {
        #[cfg(unix)]
        let codes = [
            ("EAI_NONAME", libc::EAI_NONAME, true),
            ("EAI_NODATA", libc::EAI_NODATA, true),
            ("EAI_AGAIN", libc::EAI_AGAIN, false),
            ("EAI_FAIL", libc::EAI_FAIL, false),
            ("EAI_MEMORY", libc::EAI_MEMORY, false),
            ("EAI_SYSTEM", libc::EAI_SYSTEM, false),
            ("an unknown code", -9999, false),
        ];
        #[cfg(windows)]
        let codes = [
            ("WSAHOST_NOT_FOUND", WSAHOST_NOT_FOUND, true),
            ("WSANO_DATA", WSANO_DATA, true),
            ("WSATRY_AGAIN", WSATRY_AGAIN, false),
            ("WSANO_RECOVERY", WSANO_RECOVERY, false),
            ("WSA_NOT_ENOUGH_MEMORY", WSA_NOT_ENOUGH_MEMORY, false),
            ("WSAENETDOWN", WSAENETDOWN, false),
        ];
        for (label, code, missing) in codes {
            let err = name_error(LookupError::new(code));
            assert_eq!(err.kind() == io::ErrorKind::NotFound, missing, "{label}");
            // The system's own text stays, whichever way it was classified.
            assert!(!err.to_string().is_empty(), "{label}");
        }
    }

    #[tokio::test]
    async fn the_system_resolver_resolves_localhost() {
        let name = DomainName::parse_normalised("localhost").unwrap();
        let addrs = SystemResolver.resolve(&name, 8080).await.unwrap();
        assert_ne!(addrs.len(), 0);
        assert!(
            addrs
                .iter()
                .all(|a| a.port() == 8080 && a.ip().is_loopback())
        );
    }
}
