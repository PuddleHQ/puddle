// SPDX-License-Identifier: AGPL-3.0-or-later
//! The company-network destination category: which signals the host reads, and how they turn a
//! private address into a company-network one.
//!
//! The host reads the signals (the proxy script, DNS suffixes, installed private-access clients,
//! routes); this module only decides. A signal the host cannot read answers "no", so an unknown
//! address stays in its local/private category and the company toggle never opens what the
//! private toggle guards. The LAN reading is the one signal whose "no" would widen access (it is
//! the guard that keeps a home-LAN address out of the category), so it has a third answer,
//! [`LanReading::Unread`], and that answer also keeps the address out.
//!
//! The shipped host gives [`CompanySignals`] on Windows (`puddle_host::HostSignals`); elsewhere it
//! gives none, so there the category is empty and its toggle changes nothing.

use std::collections::BTreeSet;
use std::net::{IpAddr, Ipv4Addr};

use puddle_types::CompanySignal;

/// Whether an address is on the subnet of a physical LAN interface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LanReading {
    /// On a physical LAN's own subnet: local/private, whatever else it matches.
    OnLan,
    /// The host read its interfaces and the address is on none of the physical ones.
    OffLan,
    /// The host could not read its interfaces, so it cannot rule out that the address is on the
    /// LAN. Treated like [`LanReading::OnLan`]: the address stays local/private.
    Unread,
}

/// What the host can tell about its company network. Every method but
/// [`CompanySignals::physical_lan`] answers "no" when the host cannot read the signal (the
/// defaults), so an unreadable signal never widens access. `physical_lan` answers
/// [`LanReading::Unread`] by default, because there a plain "no" would widen access.
pub trait CompanySignals: Send + Sync {
    /// Whether the proxy script answers `DIRECT` for `name` while it names proxies for other
    /// URLs.
    fn pac_direct(&self, _name: &str) -> bool {
        false
    }

    /// Whether `name` is under the machine's primary DNS suffix, a connection-specific suffix,
    /// the suffix search list or a name-resolution-policy namespace.
    fn in_dns_namespace(&self, _name: &str) -> bool {
        false
    }

    /// Whether a known private-access client (Zscaler, Netskope, WARP, Tailscale) is installed
    /// and active.
    fn private_access_client(&self) -> bool {
        false
    }

    /// Whether `ip` is routed through a VPN interface rather than the physical LAN.
    fn vpn_routed(&self, _ip: IpAddr) -> bool {
        false
    }

    /// Whether `ip` is on the subnet of a physical LAN interface. Such an address is a
    /// local/private network address, whatever else it matches; so is one the host could not
    /// tell about ([`LanReading::Unread`]).
    fn physical_lan(&self, _ip: IpAddr) -> LanReading {
        LanReading::Unread
    }

    /// What the host found, for a screen that lists it. The default knows the client flag and no
    /// suffixes; a host that reads suffixes lists them here.
    fn found(&self) -> CompanyFound {
        CompanyFound {
            dns_suffixes: Vec::new(),
            private_access_client: self.private_access_client(),
        }
    }
}

/// What a host found that drives the company-network category, in words a screen can show.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompanyFound {
    /// DNS suffixes and name-resolution namespaces of the machine, lower case, each once.
    pub dns_suffixes: Vec<String>,
    /// A private-access client (Zscaler, Netskope, WARP, Tailscale) is installed and active.
    pub private_access_client: bool,
}

/// The signals of a host that can read none: the category is empty.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoSignals;

impl CompanySignals for NoSignals {}

/// A fixed set of signals: what the host read at one moment, or a test's setup.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StaticSignals {
    pac_direct: BTreeSet<String>,
    dns_suffixes: Vec<String>,
    private_access_client: bool,
    vpn_routes: Vec<(IpAddr, u8)>,
    lan_subnets: Vec<(IpAddr, u8)>,
    lan_unread: bool,
}

fn clean(name: &str) -> String {
    name.trim_matches('.').to_ascii_lowercase()
}

impl StaticSignals {
    /// No signals read.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// `name` (exact) gets `DIRECT` from the proxy script.
    #[must_use]
    pub fn with_pac_direct(mut self, name: &str) -> Self {
        self.pac_direct.insert(clean(name));
        self
    }

    /// `suffix` is a DNS suffix or namespace of the machine: the name itself and everything under
    /// it counts. An empty suffix is ignored, so a read that found nothing never matches every
    /// name.
    #[must_use]
    pub fn with_dns_suffix(mut self, suffix: &str) -> Self {
        let suffix = clean(suffix);
        if !suffix.is_empty() {
            self.dns_suffixes.push(suffix);
        }
        self
    }

    /// A private-access client is installed and active.
    #[must_use]
    pub fn with_private_access_client(mut self) -> Self {
        self.private_access_client = true;
        self
    }

    /// `net/bits` is routed through a VPN interface.
    #[must_use]
    pub fn with_vpn_route(mut self, net: IpAddr, bits: u8) -> Self {
        self.vpn_routes.push((net, bits));
        self
    }

    /// `net/bits` is the subnet of a physical LAN interface.
    #[must_use]
    pub fn with_lan_subnet(mut self, net: IpAddr, bits: u8) -> Self {
        self.lan_subnets.push((net, bits));
        self
    }

    /// The host could not read its LAN interfaces: every address answers
    /// [`LanReading::Unread`], whatever subnets were given.
    #[must_use]
    pub fn with_lan_unread(mut self) -> Self {
        self.lan_unread = true;
        self
    }
}

impl CompanySignals for StaticSignals {
    fn pac_direct(&self, name: &str) -> bool {
        self.pac_direct.contains(&clean(name))
    }

    fn in_dns_namespace(&self, name: &str) -> bool {
        let name = clean(name);
        self.dns_suffixes.iter().any(|suffix| {
            name == *suffix
                || name
                    .strip_suffix(suffix.as_str())
                    .is_some_and(|rest| rest.ends_with('.'))
        })
    }

    fn private_access_client(&self) -> bool {
        self.private_access_client
    }

    fn found(&self) -> CompanyFound {
        let mut dns_suffixes = self.dns_suffixes.clone();
        dns_suffixes.sort();
        dns_suffixes.dedup();
        CompanyFound {
            dns_suffixes,
            private_access_client: self.private_access_client,
        }
    }

    fn vpn_routed(&self, ip: IpAddr) -> bool {
        self.vpn_routes
            .iter()
            .any(|&(net, bits)| in_net(ip, net, bits))
    }

    fn physical_lan(&self, ip: IpAddr) -> LanReading {
        if self.lan_unread {
            LanReading::Unread
        } else if self
            .lan_subnets
            .iter()
            .any(|&(net, bits)| in_net(ip, net, bits))
        {
            LanReading::OnLan
        } else {
            LanReading::OffLan
        }
    }
}

/// Whether `ip` is in `net/bits`; different address families never match.
fn in_net(ip: IpAddr, net: IpAddr, bits: u8) -> bool {
    match (ip, net) {
        (IpAddr::V4(a), IpAddr::V4(n)) if bits <= 32 => {
            let mask = u32::MAX.checked_shl(32 - u32::from(bits)).unwrap_or(0);
            a.to_bits() & mask == n.to_bits() & mask
        }
        (IpAddr::V6(a), IpAddr::V6(n)) if bits <= 128 => {
            let mask = u128::MAX.checked_shl(128 - u32::from(bits)).unwrap_or(0);
            a.to_bits() & mask == n.to_bits() & mask
        }
        _ => false,
    }
}

/// The signal that puts `ip` (with the name it was reached by, if any) in the company network,
/// or `None`. Only an address that is already in the local/private category qualifies
/// (`private` is that classification): a public address stays public whatever its name says, and
/// loopback, link-local, metadata and special-use addresses never qualify. An address on a
/// physical LAN's own subnet stays local/private, and so does any address while the host cannot
/// read its LAN interfaces.
#[must_use]
pub(crate) fn company_signal(
    private: bool,
    ip: IpAddr,
    name: Option<&str>,
    signals: &dyn CompanySignals,
) -> Option<CompanySignal> {
    if !private {
        return None;
    }
    // An IPv4 address in IPv6 clothes is judged as the IPv4 address.
    let ip = match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
        v4 @ IpAddr::V4(_) => v4,
    };
    if signals.physical_lan(ip) != LanReading::OffLan {
        return None;
    }
    let named =
        |test: fn(&dyn CompanySignals, &str) -> bool| name.is_some_and(|n| test(signals, n));
    if named(|s, n| s.pac_direct(n)) {
        return Some(CompanySignal::PacDirect);
    }
    if named(|s, n| s.in_dns_namespace(n)) {
        return Some(CompanySignal::DnsSuffix);
    }
    if let IpAddr::V4(v4) = ip {
        if is_cgnat(v4) && signals.private_access_client() {
            return Some(CompanySignal::PrivateAccessCgnat);
        }
        if v4.is_private() && signals.vpn_routed(ip) {
            return Some(CompanySignal::VpnRoute);
        }
    }
    None
}

fn is_cgnat(a: Ipv4Addr) -> bool {
    a.to_bits() & 0xFFC0_0000 == 0x6440_0000
}

#[cfg(test)]
mod tests;
