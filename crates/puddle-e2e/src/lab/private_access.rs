// SPDX-License-Identifier: AGPL-3.0-or-later
//! A private-access client's network, as the host sees it: the stand-in for Zscaler Private Access,
//! Netskope, WARP or a similar client, none of which a test can install.
//!
//! What such a client does to the host: it adds an adapter with an address in `100.64.0.0/10`,
//! routes the company's names to that range, adds the company's DNS suffix, and answers a lookup
//! of an intranet name with an address in the range. A connection to that address is carried to
//! the intranet by the client.
//!
//! The fixture has two halves:
//!
//! - **What the host reads**: [`PrivateAccess::NAME`] resolves to [`PrivateAccess::ADDRESS`]
//!   ([`PrivateAccess::answers`], for the test's resolver), and [`PrivateAccess::adapter`] says
//!   which adapter, suffix and route the client adds. A host reader that is under test gets these
//!   values from its fake OS layer; the guard gets them as its signals.
//! - **What the client carries**: the intranet site ([`WebHost`]) and a tunnel that sends
//!   [`PrivateAccess::NAME`] to it ([`PrivateAccess::tunnel_port`]). Nothing a test can bind has a
//!   `100.64.x.y` address, so a test reaches the intranet by the proxied route (the check runs on
//!   the host's own answer, the connection goes by name), which is also how a client that works as
//!   a proxy behaves.
//!
//! Nothing here changes the machine: no adapter, route, DNS setting or address is added. Test
//! code only.

use std::net::{IpAddr, Ipv4Addr};

use super::{NameProxy, Seen, WebHost, WebOptions};

/// What the client adds to the host's network, as data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Adapter {
    /// The adapter's description, as an installed client names it.
    pub description: &'static str,
    /// The address the adapter holds.
    pub address: Ipv4Addr,
    /// The DNS suffix the client adds to the machine.
    pub dns_suffix: &'static str,
    /// The range the adapter routes (network, prefix length).
    pub route: (Ipv4Addr, u8),
}

/// The private-access stand-in. Stops when dropped.
pub struct PrivateAccess {
    web: WebHost,
    tunnel: NameProxy,
}

impl PrivateAccess {
    /// An intranet name behind the client, under its DNS suffix.
    pub const NAME: &'static str = "intranet.corp.test";
    /// What the client's DNS answers for [`Self::NAME`]: an address in `100.64.0.0/10`.
    pub const ADDRESS: Ipv4Addr = Ipv4Addr::new(100, 64, 12, 7);
    /// A host on the physical LAN (a printer, a router): private, and not behind the client.
    pub const LAN_ADDRESS: Ipv4Addr = Ipv4Addr::new(192, 168, 77, 20);

    /// Starts the intranet site and the tunnel to it.
    pub async fn start() -> Self {
        let web = WebHost::start(&[Self::NAME], WebOptions::default()).await;
        let tunnel = NameProxy::start(&[]).await;
        web.register(&tunnel, &[Self::NAME]);
        Self { web, tunnel }
    }

    /// The adapter, suffix and route the client adds.
    #[must_use]
    pub fn adapter() -> Adapter {
        Adapter {
            description: "Lab Private Access Adapter",
            address: Ipv4Addr::new(100, 64, 0, 2),
            dns_suffix: "corp.test",
            route: (Ipv4Addr::new(100, 64, 0, 0), 10),
        }
    }

    /// The names the host resolves through the client, with their addresses.
    #[must_use]
    pub fn answers() -> Vec<(&'static str, IpAddr)> {
        vec![(Self::NAME, IpAddr::V4(Self::ADDRESS))]
    }

    /// The port of the tunnel (on `127.0.0.1`): a proxy hop that carries the intranet name to the
    /// site.
    #[must_use]
    pub fn tunnel_port(&self) -> u16 {
        self.tunnel.port()
    }

    /// The `CONNECT` targets the tunnel carried (`host:port`).
    #[must_use]
    pub fn carried(&self) -> Vec<String> {
        self.tunnel.connects()
    }

    /// The requests the intranet site answered.
    #[must_use]
    pub fn served(&self) -> Vec<Seen> {
        self.web.seen()
    }
}

#[cfg(test)]
mod tests;
