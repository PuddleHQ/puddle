// SPDX-License-Identifier: AGPL-3.0-or-later
//! Puddle's own listeners: every Puddle listener registers here when it binds, and
//! the guard blocks a workspace from reaching any of them, whatever the loopback toggle or
//! the rules say.

use std::fmt;
use std::io;
use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, PoisonError, RwLock};

/// What a registered Puddle listener is, for the block message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum EndpointKind {
    /// The local API.
    Api,
    /// The image-pull proxy.
    PullProxy,
    /// The browser front for VS Code in the browser.
    BrowserFront,
    /// A pinned port forward.
    PinnedForward,
    /// The API's listener for a reverse proxy on another machine.
    RemoteApi,
    /// The reverse proxy in front of Puddle (its port on every own address, or its public name).
    ExternalFront,
    /// Any other listener.
    Other,
}

impl EndpointKind {
    /// A short description for messages.
    #[must_use]
    pub const fn describe(self) -> &'static str {
        match self {
            Self::Api => "API",
            Self::PullProxy => "image-pull proxy",
            Self::BrowserFront => "browser front",
            Self::PinnedForward => "pinned port forward",
            Self::RemoteApi => "remote API listener",
            Self::ExternalFront => "reverse proxy in front of Puddle",
            Self::Other => "listener",
        }
    }
}

impl fmt::Display for EndpointKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.describe())
    }
}

/// Which addresses land on this host. A seam, so tests don't depend on the machine's interfaces.
pub trait HostAddrs: Send + Sync {
    /// Whether a connection to `ip` reaches this host's own listeners.
    fn is_own(&self, ip: IpAddr) -> bool;
}

/// The real [`HostAddrs`]: loopback and unspecified addresses land here, and so does any
/// address the host can `bind`, which is exactly the set of its own interface addresses (so a
/// listener on `0.0.0.0` reached through the host's LAN address is caught too).
#[derive(Debug, Clone, Copy, Default)]
pub struct OwnAddresses;

impl HostAddrs for OwnAddresses {
    fn is_own(&self, ip: IpAddr) -> bool {
        is_own_with(ip, |ip| UdpSocket::bind((ip, 0)).map(drop))
    }
}

/// [`OwnAddresses::is_own`] with the bind attempt passed in, so a test can make it fail.
fn is_own_with(ip: IpAddr, bind: impl FnOnce(IpAddr) -> io::Result<()>) -> bool {
    ip.is_loopback() || ip.is_unspecified() || bind_means_own(bind(ip))
}

/// Only `AddrNotAvailable` says the host does not own the address. Any other failure (out of
/// file descriptors, a permission error) is treated as owned, so the guard fails closed.
fn bind_means_own(bind: io::Result<()>) -> bool {
    match bind {
        Ok(()) => true,
        Err(e) => e.kind() != io::ErrorKind::AddrNotAvailable,
    }
}

#[derive(Debug)]
struct Entry {
    id: u64,
    addr: SocketAddr,
    kind: EndpointKind,
}

#[derive(Debug)]
struct NameEntry {
    id: u64,
    name: String,
    subdomains: bool,
    kind: EndpointKind,
}

#[derive(Debug, Default)]
struct Inner {
    entries: RwLock<Vec<Entry>>,
    names: RwLock<Vec<NameEntry>>,
    next_id: AtomicU64,
}

/// The registry of Puddle's own listeners. Cheap to clone; clones share the registry.
///
/// ```
/// use puddle_netpolicy::{EndpointKind, OwnAddresses, PuddleEndpoints};
///
/// let endpoints = PuddleEndpoints::new();
/// let api = endpoints.register("127.0.0.1:7070".parse().unwrap(), EndpointKind::Api);
/// assert_eq!(endpoints.find("127.0.0.1:7070".parse().unwrap(), &OwnAddresses), Some(EndpointKind::Api));
/// drop(api);
/// assert_eq!(endpoints.find("127.0.0.1:7070".parse().unwrap(), &OwnAddresses), None);
/// ```
#[derive(Debug, Clone, Default)]
pub struct PuddleEndpoints {
    inner: Arc<Inner>,
}

/// A listener's registration; dropping it unregisters the listener. Keep it as long as the
/// listener is bound.
#[derive(Debug)]
#[must_use = "dropping the registration unregisters the listener at once"]
pub struct Registration {
    endpoints: PuddleEndpoints,
    id: u64,
}

impl Drop for Registration {
    fn drop(&mut self) {
        let mut entries = self
            .endpoints
            .inner
            .entries
            .write()
            .unwrap_or_else(PoisonError::into_inner);
        entries.retain(|e| e.id != self.id);
    }
}

/// A blocked name's registration; dropping it lifts the block. Keep it as long as the name must
/// stay blocked.
#[derive(Debug)]
#[must_use = "dropping the registration lifts the block at once"]
pub struct NameRegistration {
    endpoints: PuddleEndpoints,
    id: u64,
}

impl Drop for NameRegistration {
    fn drop(&mut self) {
        let mut names = self
            .endpoints
            .inner
            .names
            .write()
            .unwrap_or_else(PoisonError::into_inner);
        names.retain(|e| e.id != self.id);
    }
}

impl PuddleEndpoints {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a listener bound at `addr` (with its real port, after `bind`).
    pub fn register(&self, addr: SocketAddr, kind: EndpointKind) -> Registration {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        self.inner
            .entries
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Entry { id, addr, kind });
        Registration {
            endpoints: self.clone(),
            id,
        }
    }

    /// Blocks a public name (and, with `subdomains`, every name below it) for guests whatever its
    /// address is. A reverse proxy off this machine is reached by name, so no port or address of
    /// this host stands for it.
    pub fn block_name(&self, name: &str, subdomains: bool, kind: EndpointKind) -> NameRegistration {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        self.inner
            .names
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .push(NameEntry {
                id,
                name: name.trim_end_matches('.').to_ascii_lowercase(),
                subdomains,
                kind,
            });
        NameRegistration {
            endpoints: self.clone(),
            id,
        }
    }

    /// What a guest asking for `name` would reach, when the name is blocked.
    #[must_use]
    pub fn find_name(&self, name: &str) -> Option<EndpointKind> {
        let name = name.trim_end_matches('.').to_ascii_lowercase();
        self.inner
            .names
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .find(|e| {
                e.name == name
                    || (e.subdomains
                        && name
                            .strip_suffix(e.name.as_str())
                            .is_some_and(|rest| rest.ends_with('.')))
            })
            .map(|e| e.kind)
    }

    /// The listener a connection to `dest` would reach, if any: same port, and an address that
    /// is the listener's own or lands on this host (loopback, unspecified, one of its interface
    /// addresses, also as IPv4-mapped IPv6). Over-matching a listener bound to one address only
    /// is deliberate: it fails closed.
    #[must_use]
    pub fn find(&self, dest: SocketAddr, host: &dyn HostAddrs) -> Option<EndpointKind> {
        let entries = self
            .inner
            .entries
            .read()
            .unwrap_or_else(PoisonError::into_inner);
        let mut same_port = entries.iter().filter(|e| e.addr.port() == dest.port());
        let first = same_port.next()?;
        let ip = unmapped(dest.ip());
        if let Some(e) = std::iter::once(first)
            .chain(same_port)
            .find(|e| unmapped(e.addr.ip()) == ip)
        {
            return Some(e.kind);
        }
        (ip.is_loopback() || ip.is_unspecified() || host.is_own(ip)).then_some(first.kind)
    }

    /// The registered listeners, in registration order.
    #[must_use]
    pub fn list(&self) -> Vec<(SocketAddr, EndpointKind)> {
        self.inner
            .entries
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|e| (e.addr, e.kind))
            .collect()
    }
}

fn unmapped(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
        IpAddr::V4(_) => ip,
    }
}

#[cfg(test)]
mod tests;
