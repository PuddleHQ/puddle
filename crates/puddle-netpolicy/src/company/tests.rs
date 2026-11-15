// SPDX-License-Identifier: AGPL-3.0-or-later
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
    assert_eq!(s.physical_lan(ip("192.168.1.77")), LanReading::OnLan);
    assert_eq!(s.physical_lan(ip("192.168.2.1")), LanReading::OffLan);
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
#[test]
fn a_prefix_longer_than_the_address_matches_nothing() {
    let s = StaticSignals::new()
        .with_vpn_route(ip("10.0.0.0"), 33)
        .with_vpn_route(ip("fd00::"), 129);
    assert!(!s.vpn_routed(ip("10.0.0.1")));
    assert!(!s.vpn_routed(ip("fd00::1")));
}

/// A host that can read only the proxy script and its LAN interfaces (none has a private
/// subnet): the signals it cannot read answer "no".
struct PacOnly;

impl CompanySignals for PacOnly {
    fn pac_direct(&self, name: &str) -> bool {
        name == "nexus.corp.example"
    }

    fn physical_lan(&self, _ip: IpAddr) -> LanReading {
        LanReading::OffLan
    }
}

/// A host whose proxy script says everything is direct but that could not read its interfaces.
struct PacButNoLan;

impl CompanySignals for PacButNoLan {
    fn pac_direct(&self, _name: &str) -> bool {
        true
    }
}

#[test]
fn an_unread_lan_keeps_every_private_address_out_of_the_company_network() {
    // A trait default that says "not on the LAN" would put a home address in the category.
    for addr in ["192.168.1.5", "10.1.2.3", "100.64.3.7"] {
        assert_eq!(
            company_signal(true, ip(addr), Some("nas.home.test"), &PacButNoLan),
            None,
            "{addr}"
        );
    }
    assert_eq!(
        PacButNoLan.physical_lan(ip("192.168.1.5")),
        LanReading::Unread
    );
    let unread = StaticSignals::new()
        .with_pac_direct("nas.home.test")
        .with_lan_subnet(ip("10.0.0.0"), 8)
        .with_lan_unread();
    assert_eq!(unread.physical_lan(ip("172.16.0.1")), LanReading::Unread);
    assert_eq!(
        company_signal(true, ip("172.16.0.1"), Some("nas.home.test"), &unread),
        None
    );
}

#[test]
fn a_signal_the_host_cannot_read_never_widens_access() {
    let signal = |addr: &str, name: &str| company_signal(true, ip(addr), Some(name), &PacOnly);
    assert_eq!(
        signal("10.1.2.3", "nexus.corp.example"),
        Some(CompanySignal::PacDirect)
    );
    // Not on a physical LAN as far as it can tell, but no other signal either.
    assert_eq!(signal("100.64.3.7", "other.corp.example"), None);
    assert_eq!(signal("10.8.0.9", "other.corp.example"), None);
}

#[test]
fn what_a_host_found_lists_each_suffix_once_and_the_client() {
    let s = StaticSignals::new()
        .with_dns_suffix("Corp.Example.")
        .with_dns_suffix("corp.example")
        .with_dns_suffix("ad.example")
        .with_private_access_client();
    assert_eq!(
        s.found(),
        CompanyFound {
            dns_suffixes: vec!["ad.example".into(), "corp.example".into()],
            private_access_client: true,
        }
    );
}

#[test]
fn a_host_that_reads_nothing_found_nothing() {
    assert_eq!(PacOnly.found(), CompanyFound::default());
    assert_eq!(NoSignals.found(), CompanyFound::default());
}

/// A host that reads only whether a private-access client is active.
struct ClientOnly;

impl CompanySignals for ClientOnly {
    fn private_access_client(&self) -> bool {
        true
    }
}

#[test]
fn the_default_found_reports_the_client_the_host_read_and_no_suffixes() {
    assert_eq!(
        ClientOnly.found(),
        CompanyFound {
            dns_suffixes: Vec::new(),
            private_access_client: true,
        }
    );
}

#[test]
fn a_host_that_implements_nothing_answers_no_to_every_signal_and_unread_to_the_lan() {
    let none = NoSignals;
    assert!(!none.pac_direct("nexus.corp.example"));
    assert!(!none.in_dns_namespace("nexus.corp.example"));
    assert!(!none.private_access_client());
    assert!(!none.vpn_routed(ip("10.8.0.1")));
    assert_eq!(none.physical_lan(ip("10.8.0.1")), LanReading::Unread);
}

#[test]
fn a_private_address_with_no_signal_on_a_read_lan_stays_private() {
    let s = StaticSignals::new().with_lan_subnet(ip("192.168.1.0"), 24);
    assert_eq!(
        company_signal(true, ip("10.1.2.3"), Some("a.test"), &s),
        None
    );
    assert_eq!(company_signal(true, ip("172.16.0.9"), None, &s), None);
}

#[test]
fn a_native_ipv6_private_address_has_no_address_signal_but_a_name_still_counts() {
    // The CGNAT and VPN-route signals are IPv4 notions; a unique-local IPv6 address only
    // qualifies through its name.
    let s = StaticSignals::new()
        .with_dns_suffix("corp.example")
        .with_private_access_client()
        .with_vpn_route(ip("10.0.0.0"), 8);
    assert_eq!(company_signal(true, ip("fd00::1"), None, &s), None);
    assert_eq!(
        company_signal(true, ip("fd00::1"), Some("host.corp.example"), &s),
        Some(CompanySignal::DnsSuffix)
    );
}
