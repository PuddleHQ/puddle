// SPDX-License-Identifier: GPL-3.0-or-later
//! What the machine's network looks like, in neutral terms: its adapters, its IPv4 routes and the
//! DNS namespaces it resolves through. The company-network category reads it to tell a company
//! address (behind a VPN or a private-access client, under a company DNS suffix) from a home or
//! office LAN. Only [`NetworkOs`] talks to the operating system, so everything that decides on
//! these answers is tested with [`FakeNetwork`] or a literal [`NetworkSnapshot`].
//!
//! Everything here is read-only: nothing is created, changed or sent. Windows reads
//! `GetAdaptersAddresses`, the IPv4 forwarding table and a few registry values; Unix says it does
//! not read the network (the category stays empty there).

use std::net::IpAddr;
use std::sync::Arc;
#[cfg(any(test, feature = "testing"))]
use std::sync::{Mutex, PoisonError};

/// What kind of link an adapter is, as far as the operating system says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdapterKind {
    /// Wired Ethernet, also the kind virtual Ethernet switches report.
    Ethernet,
    /// Wi-Fi.
    Wireless,
    /// A mobile broadband modem.
    Cellular,
    /// A point-to-point link: dial-up and the built-in VPN protocols.
    PointToPoint,
    /// A tunnel interface (IP-in-IP, Teredo, and what many VPN clients register).
    Tunnel,
    /// The loopback interface.
    Loopback,
    /// Anything else.
    Other,
}

impl AdapterKind {
    /// The kind of an IANA `ifType` value, which Windows reports as `IfType`.
    #[cfg(any(windows, test))]
    pub(crate) fn from_if_type(if_type: u32) -> Self {
        match if_type {
            6 => Self::Ethernet,
            71 => Self::Wireless,
            243 | 244 => Self::Cellular,
            23 | 108 => Self::PointToPoint,
            131 => Self::Tunnel,
            24 => Self::Loopback,
            _ => Self::Other,
        }
    }
}

/// One network adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Adapter {
    /// The interface index routes refer to.
    pub index: u32,
    /// The name the user sees ("Ethernet", "Wi-Fi", "Zscaler Network Adapter").
    pub name: String,
    /// The driver's description ("Intel(R) Ethernet", "Cloudflare WARP Virtual Network Adapter").
    pub description: String,
    /// What kind of link it is.
    pub kind: AdapterKind,
    /// Whether the link is up.
    pub up: bool,
    /// The connection-specific DNS suffix, if the connection has one.
    pub dns_suffix: Option<String>,
    /// The subnets of the adapter's own addresses (address and on-link prefix length).
    pub subnets: Vec<(IpAddr, u8)>,
    /// The adapter's IPv4 interface metric, added to a route's metric.
    pub metric: u32,
}

/// One IPv4 route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Route {
    /// The destination network.
    pub net: IpAddr,
    /// The prefix length.
    pub bits: u8,
    /// The adapter it leaves by ([`Adapter::index`]).
    pub interface: u32,
    /// The route's own metric.
    pub metric: u32,
}

/// What one read of the network found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NetworkSnapshot {
    /// Every adapter, up or down.
    pub adapters: Vec<Adapter>,
    /// The IPv4 routing table.
    pub routes: Vec<Route>,
    /// DNS namespaces that do not belong to one adapter: the primary DNS suffix, the suffix
    /// search list (also as group policy sets it) and the namespaces of the name-resolution
    /// policy table. Text as the machine holds it: case, dots and wildcards are the reader's to
    /// normalise.
    pub dns_namespaces: Vec<String>,
}

/// The network could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("reading the network failed: {0}")]
pub struct NetworkError(pub String);

/// What the company-network category asks of the operating system. Blocking and cheap enough to
/// call every few seconds.
pub trait NetworkOs: Send + Sync + std::fmt::Debug {
    /// Reads the adapters, the routes and the DNS namespaces.
    ///
    /// # Errors
    /// [`NetworkError`] when the adapters or the routes cannot be read. A DNS namespace that is
    /// not set, or cannot be read, is simply missing from the snapshot.
    fn read(&self) -> Result<NetworkSnapshot, NetworkError>;
}

/// The operating system's network: Windows' own answers there, "cannot read" elsewhere.
#[must_use]
pub fn system_network() -> Arc<dyn NetworkOs> {
    #[cfg(windows)]
    {
        Arc::new(crate::windows::WinNetwork)
    }
    #[cfg(not(windows))]
    {
        Arc::new(UnreadNetwork)
    }
}

/// A network this platform has no reader for.
#[cfg(any(not(windows), test))]
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct UnreadNetwork;

#[cfg(any(not(windows), test))]
impl NetworkOs for UnreadNetwork {
    fn read(&self) -> Result<NetworkSnapshot, NetworkError> {
        Err(NetworkError(
            "this platform has no network reader".to_owned(),
        ))
    }
}

/// A scripted [`NetworkOs`]: the answer is set by the test, reads are counted (feature
/// `testing`).
#[cfg(any(test, feature = "testing"))]
#[derive(Debug)]
pub struct FakeNetwork {
    answer: Mutex<Result<NetworkSnapshot, NetworkError>>,
    reads: Mutex<usize>,
}

#[cfg(any(test, feature = "testing"))]
impl FakeNetwork {
    /// A fake that reports `snapshot`.
    #[must_use]
    pub fn new(snapshot: NetworkSnapshot) -> Arc<Self> {
        Arc::new(Self {
            answer: Mutex::new(Ok(snapshot)),
            reads: Mutex::new(0),
        })
    }

    /// A fake whose reads fail with `why`.
    #[must_use]
    pub fn failing(why: &str) -> Arc<Self> {
        Arc::new(Self {
            answer: Mutex::new(Err(NetworkError(why.to_owned()))),
            reads: Mutex::new(0),
        })
    }

    /// Changes what later reads report.
    pub fn set(&self, snapshot: NetworkSnapshot) {
        *self.answer.lock().unwrap_or_else(PoisonError::into_inner) = Ok(snapshot);
    }

    /// How many times the network was read.
    #[must_use]
    pub fn reads(&self) -> usize {
        *self.reads.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(any(test, feature = "testing"))]
impl NetworkOs for FakeNetwork {
    fn read(&self) -> Result<NetworkSnapshot, NetworkError> {
        *self.reads.lock().unwrap_or_else(PoisonError::into_inner) += 1;
        self.answer
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

#[cfg(test)]
mod tests;
