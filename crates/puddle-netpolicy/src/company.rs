// SPDX-License-Identifier: GPL-3.0-or-later
//! The company-network destination category: which signals the host reads, and how they turn a
//! private address into a company-network one.
//!
//! The host reads the signals (the proxy script, DNS suffixes, installed private-access clients,
//! routes); this module only decides. A signal the host cannot read answers "no", so an unknown
//! address stays in its local/private category and the company toggle never opens what the
//! private toggle guards.

use std::collections::BTreeSet;
use std::net::{IpAddr, Ipv4Addr};

use puddle_types::CompanySignal;

/// What the host can tell about its company network. Every method answers "no" when the host
/// cannot read the signal (the defaults), so an unreadable signal never widens access.
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
    /// local/private network address, whatever else it matches.
    fn on_physical_lan(&self, _ip: IpAddr) -> bool {
        false
    }
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

    fn vpn_routed(&self, ip: IpAddr) -> bool {
        self.vpn_routes
            .iter()
            .any(|&(net, bits)| in_net(ip, net, bits))
    }

    fn on_physical_lan(&self, ip: IpAddr) -> bool {
        self.lan_subnets
            .iter()
            .any(|&(net, bits)| in_net(ip, net, bits))
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
/// physical LAN's own subnet stays local/private.
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
    if signals.on_physical_lan(ip) {
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
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn dns_namespace_matches_the_suffix_itself_and_names_under_it_only() {
        let s = StaticSignals::new().with_dns_suffix(".Corp.Example.");
        assert!(s.in_dns_namespace("corp.example"));
        assert!(s.in_dns_namespace("nexus.corp.example"));
        assert!(s.in_dns_namespace("a.b.CORP.example."));
        assert!(!s.in_dns_namespace("notcorp.example"));
        assert!(!s.in_dns_namespace("corp.example.evil.test"));
        assert!(!s.in_dns_namespace("example"));
    }

    #[test]
    fn an_empty_suffix_never_matches() {
        let s = StaticSignals::new()
            .with_dns_suffix("")
            .with_dns_suffix(".");
        assert!(!s.in_dns_namespace("anything.example"));
        assert!(!s.in_dns_namespace(""));
    }

    #[test]
    fn routes_and_subnets_match_their_family_and_prefix_only() {
        let s = StaticSignals::new()
            .with_vpn_route(ip("10.8.0.0"), 16)
            .with_vpn_route(ip("fd00::"), 8)
            .with_lan_subnet(ip("192.168.1.0"), 24);
        assert!(s.vpn_routed(ip("10.8.255.255")));
        assert!(!s.vpn_routed(ip("10.9.0.0")));
        assert!(s.vpn_routed(ip("fd12::1")));
        assert!(!s.vpn_routed(ip("fe80::1")));
        assert!(s.on_physical_lan(ip("192.168.1.77")));
        assert!(!s.on_physical_lan(ip("192.168.2.1")));
        // A /0 route covers its whole family and nothing of the other.
        let all = StaticSignals::new().with_vpn_route(ip("0.0.0.0"), 0);
        assert!(all.vpn_routed(ip("10.1.1.1")));
        assert!(!all.vpn_routed(ip("::1")));
    }

    #[test]
    fn pac_direct_is_an_exact_case_insensitive_name() {
        let s = StaticSignals::new().with_pac_direct("Intranet.corp.test");
        assert!(s.pac_direct("intranet.corp.test"));
        assert!(!s.pac_direct("x.intranet.corp.test"));
    }

    #[test]
    fn no_signals_never_qualify() {
        let none = NoSignals;
        for addr in ["10.1.2.3", "100.64.3.7", "fd00::1"] {
            assert_eq!(
                company_signal(true, ip(addr), Some("nexus.corp.example"), &none),
                None
            );
        }
    }

    #[test]
    fn signals_are_tried_in_the_documented_order() {
        let all = StaticSignals::new()
            .with_pac_direct("nexus.corp.example")
            .with_dns_suffix("corp.example")
            .with_private_access_client()
            .with_vpn_route(ip("100.64.0.0"), 10)
            .with_vpn_route(ip("10.0.0.0"), 8);
        let signal = |addr: &str, name: Option<&str>| company_signal(true, ip(addr), name, &all);
        assert_eq!(
            signal("100.64.3.7", Some("nexus.corp.example")),
            Some(CompanySignal::PacDirect)
        );
        assert_eq!(
            signal("100.64.3.7", Some("other.corp.example")),
            Some(CompanySignal::DnsSuffix)
        );
        assert_eq!(
            signal("100.64.3.7", None),
            Some(CompanySignal::PrivateAccessCgnat)
        );
        assert_eq!(signal("10.2.3.4", None), Some(CompanySignal::VpnRoute));
    }

    #[test]
    fn cgnat_needs_the_client_and_the_vpn_signal_needs_rfc1918() {
        let no_client = StaticSignals::new().with_vpn_route(ip("100.64.0.0"), 10);
        assert_eq!(
            company_signal(true, ip("100.64.3.7"), None, &no_client),
            None
        );
        let client = StaticSignals::new().with_private_access_client();
        assert_eq!(
            company_signal(true, ip("100.64.3.7"), None, &client),
            Some(CompanySignal::PrivateAccessCgnat)
        );
        // The edges of 100.64.0.0/10.
        assert_eq!(
            company_signal(true, ip("100.63.255.255"), None, &client),
            None
        );
        assert_eq!(company_signal(true, ip("100.128.0.0"), None, &client), None);
        // A client says nothing about RFC 1918 addresses.
        assert_eq!(company_signal(true, ip("10.1.1.1"), None, &client), None);
    }

    #[test]
    fn the_physical_lan_wins_over_every_signal() {
        let s = StaticSignals::new()
            .with_pac_direct("nas.home.test")
            .with_dns_suffix("home.test")
            .with_vpn_route(ip("192.168.0.0"), 16)
            .with_lan_subnet(ip("192.168.1.0"), 24);
        assert_eq!(
            company_signal(true, ip("192.168.1.5"), Some("nas.home.test"), &s),
            None
        );
        assert_eq!(
            company_signal(true, ip("192.168.7.5"), Some("nas.home.test"), &s),
            Some(CompanySignal::PacDirect)
        );
    }

    #[test]
    fn only_a_private_class_address_qualifies() {
        let s = StaticSignals::new()
            .with_dns_suffix("corp.example")
            .with_pac_direct("pub.corp.example");
        assert_eq!(
            company_signal(false, ip("8.8.8.8"), Some("pub.corp.example"), &s),
            None
        );
    }

    #[test]
    fn an_ipv4_mapped_address_is_judged_as_the_ipv4_address() {
        let s = StaticSignals::new().with_vpn_route(ip("10.8.0.0"), 16);
        assert_eq!(
            company_signal(true, ip("::ffff:10.8.1.1"), None, &s),
            Some(CompanySignal::VpnRoute)
        );
        assert_eq!(company_signal(true, ip("::ffff:10.9.1.1"), None, &s), None);
    }
}
