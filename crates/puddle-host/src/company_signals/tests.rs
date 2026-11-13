// SPDX-License-Identifier: AGPL-3.0-or-later
//! The company-network readers over fixtures: literal snapshots of a home network, a corporate VPN
//! and a private-access client, so no test depends on the machine it runs on.

use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicUsize, Ordering};

use puddle_e2e::lab::PrivateAccess;
use puddle_netpolicy::{
    AddressClass, CompanySignal, LocalAccess, LocalCategory, NetPolicy, classify_with_signals,
};
use puddle_types::Host;
use puddle_upstream::{FakeNetwork, FakeOs, Hop, NetworkRoute, PacError, ProxyAddr, ProxyConfig};

use super::pac::Spawn;
use super::*;

fn ip(text: &str) -> IpAddr {
    text.parse().expect("an address")
}

fn adapter(index: u32, name: &str, kind: AdapterKind, subnet: &str, bits: u8) -> Adapter {
    Adapter {
        index,
        name: name.to_owned(),
        description: name.to_owned(),
        kind,
        up: true,
        dns_suffix: None,
        subnets: vec![(ip(subnet), bits)],
        metric: 25,
    }
}

fn route(net: &str, bits: u8, interface: u32, metric: u32) -> NetworkRoute {
    NetworkRoute {
        net: ip(net),
        bits,
        interface,
        metric,
    }
}

/// A machine at home: Wi-Fi on 192.168.1.0/24 and loopback.
fn home() -> NetworkSnapshot {
    let mut wifi = adapter(7, "Wi-Fi", AdapterKind::Wireless, "192.168.1.20", 24);
    wifi.dns_suffix = Some("fritz.box".to_owned());
    NetworkSnapshot {
        adapters: vec![
            adapter(
                1,
                "Loopback Pseudo-Interface 1",
                AdapterKind::Loopback,
                "127.0.0.1",
                8,
            ),
            wifi,
        ],
        routes: vec![
            route("0.0.0.0", 0, 7, 25),
            route("192.168.1.0", 24, 7, 281),
            route("127.0.0.0", 8, 1, 331),
        ],
        dns_namespaces: Vec::new(),
    }
}

/// The home machine with a corporate VPN (a point-to-point link) that routes 10.20.0.0/16 and, more
/// specifically, a 10.20.5.0/24 stays on the home side.
fn with_vpn() -> NetworkSnapshot {
    let mut snapshot = home();
    let mut vpn = adapter(12, "Contoso VPN", AdapterKind::PointToPoint, "10.8.0.6", 32);
    vpn.dns_suffix = Some("corp.contoso.example".to_owned());
    snapshot.adapters.push(vpn);
    snapshot.routes.push(route("10.20.0.0", 16, 12, 26));
    snapshot.routes.push(route("10.20.5.0", 24, 7, 26));
    snapshot
}

fn signals(snapshot: NetworkSnapshot) -> HostSignals {
    HostSignals::new(FakeNetwork::new(snapshot), None)
}

#[test]
fn a_home_machine_has_no_company_signals_and_its_lan_stays_the_lan() {
    let signals = signals(home());
    assert_eq!(signals.physical_lan(ip("192.168.1.77")), LanReading::OnLan);
    assert_eq!(signals.physical_lan(ip("10.0.0.1")), LanReading::OffLan);
    assert!(!signals.private_access_client());
    assert!(!signals.vpn_routed(ip("10.20.3.4")));
    assert!(!signals.vpn_routed(ip("192.168.1.77")));
}

#[test]
fn an_address_routed_through_the_vpn_is_vpn_routed() {
    let signals = signals(with_vpn());
    assert!(signals.vpn_routed(ip("10.20.3.4")));
    // Not covered by the VPN's route: the default route leaves by Wi-Fi.
    assert!(!signals.vpn_routed(ip("10.30.0.1")));
    // The longer prefix wins: 10.20.5.0/24 is on the home side.
    assert!(!signals.vpn_routed(ip("10.20.5.9")));
    // The VPN is a VPN, not a private-access client.
    assert!(!signals.private_access_client());
}

#[test]
fn a_full_tunnel_vpn_routes_what_no_closer_route_claims_but_not_the_lan() {
    let mut snapshot = with_vpn();
    // The VPN takes the default route with a lower metric than Wi-Fi's.
    snapshot.routes.push(route("0.0.0.0", 0, 12, 1));
    let signals = signals(snapshot);
    assert!(signals.vpn_routed(ip("10.99.0.1")));
    assert!(
        !signals.vpn_routed(ip("192.168.1.77")),
        "the LAN's own route is closer"
    );
    assert_eq!(signals.physical_lan(ip("192.168.1.77")), LanReading::OnLan);
}

#[test]
fn between_routes_of_one_length_the_lower_total_metric_wins() {
    let mut snapshot = with_vpn();
    snapshot.routes.push(route("10.50.0.0", 16, 12, 100));
    snapshot.routes.push(route("10.50.0.0", 16, 7, 10));
    assert!(!signals(snapshot.clone()).vpn_routed(ip("10.50.1.1")));
    // Raise Wi-Fi's own metric so the VPN's route costs less in total.
    snapshot.adapters[1].metric = 500;
    assert!(signals(snapshot).vpn_routed(ip("10.50.1.1")));
}

#[test]
fn a_vpn_that_is_down_adds_nothing() {
    assert!(
        signals(with_vpn()).vpn_routed(ip("10.20.3.4")),
        "control: the VPN is up"
    );
    let mut snapshot = with_vpn();
    snapshot.adapters[2].up = false;
    let signals = signals(snapshot);
    assert!(!signals.vpn_routed(ip("10.20.3.4")));
    assert!(!signals.in_dns_namespace("git.corp.contoso.example"));
}

#[test]
fn a_route_through_an_adapter_the_read_does_not_list_counts_for_nothing() {
    let mut snapshot = home();
    snapshot.routes.push(route("10.20.0.0", 16, 99, 1));
    assert!(!signals(snapshot).vpn_routed(ip("10.20.1.1")));
}

#[test]
fn only_ipv4_addresses_and_routes_count_for_the_vpn() {
    let mut snapshot = with_vpn();
    snapshot.routes.push(route("fd00::", 8, 12, 1));
    let signals = signals(snapshot);
    assert!(!signals.vpn_routed(ip("fd00::1")));
}

#[test]
fn a_name_under_a_connection_suffix_is_in_the_dns_namespace() {
    let signals = signals(with_vpn());
    assert!(signals.in_dns_namespace("git.corp.contoso.example"));
    assert!(signals.in_dns_namespace("CORP.contoso.example."));
    assert!(!signals.in_dns_namespace("contoso.example"));
    assert!(!signals.in_dns_namespace("evilcorp.contoso.example.attacker.test"));
    // The home router's suffix is a suffix too; only a private address behind it is ever affected.
    assert!(signals.in_dns_namespace("nas.fritz.box"));
}

#[test]
fn namespaces_from_the_machine_policy_join_the_suffixes() {
    let mut snapshot = home();
    snapshot.dns_namespaces = vec![
        "corp.example".to_owned(),
        ".nrpt.example".to_owned(),
        "*.wild.example".to_owned(),
        String::new(),
        ".".to_owned(),
        "not a name".to_owned(),
        "corp.example".to_owned(),
    ];
    let signals = signals(snapshot);
    assert!(signals.in_dns_namespace("a.corp.example"));
    assert!(signals.in_dns_namespace("a.nrpt.example"));
    assert!(signals.in_dns_namespace("a.wild.example"));
    assert!(
        !signals.in_dns_namespace("anything.else.example"),
        "an empty namespace matches nothing"
    );
    assert_eq!(
        signals.found().dns_suffixes,
        ["corp.example", "fritz.box", "nrpt.example", "wild.example"]
    );
}

#[test]
fn loopback_contributes_neither_a_lan_nor_a_suffix() {
    let mut snapshot = home();
    snapshot.adapters[0].dns_suffix = Some("loop.example".to_owned());
    let signals = signals(snapshot);
    assert!(!signals.in_dns_namespace("a.loop.example"));
    assert_eq!(signals.physical_lan(ip("127.0.0.1")), LanReading::OffLan);
}

#[test]
fn adapters_are_told_apart_by_kind_and_by_what_they_call_themselves() {
    let ethernet = |name: &str| adapter(1, name, AdapterKind::Ethernet, "10.0.0.2", 24);
    let cases = [
        ("Ethernet", AdapterKind::Ethernet, Role::Lan),
        ("Intel(R) Wi-Fi 6 AX201", AdapterKind::Wireless, Role::Lan),
        ("Mobile broadband", AdapterKind::Cellular, Role::Lan),
        ("vEthernet (WSL)", AdapterKind::Ethernet, Role::Lan),
        (
            "Zscaler Network Adapter 1.0.0",
            AdapterKind::Ethernet,
            Role::PrivateAccess,
        ),
        (
            "Netskope Adapter",
            AdapterKind::Ethernet,
            Role::PrivateAccess,
        ),
        ("CloudflareWARP", AdapterKind::Other, Role::PrivateAccess),
        ("Tailscale Tunnel", AdapterKind::Other, Role::PrivateAccess),
        (
            "Microsoft Entra Private Access",
            AdapterKind::Other,
            Role::PrivateAccess,
        ),
        (
            "Global Secure Access Tunnel",
            AdapterKind::Tunnel,
            Role::PrivateAccess,
        ),
        ("TAP-Windows Adapter V9", AdapterKind::Ethernet, Role::Vpn),
        (
            "Cisco AnyConnect Secure Mobility Client",
            AdapterKind::Ethernet,
            Role::Vpn,
        ),
        (
            "PANGP Virtual Ethernet Adapter",
            AdapterKind::Ethernet,
            Role::Vpn,
        ),
        ("WireGuard Tunnel", AdapterKind::Other, Role::Vpn),
        ("Work connection", AdapterKind::PointToPoint, Role::Vpn),
        (
            "Teredo Tunneling Pseudo-Interface",
            AdapterKind::Tunnel,
            Role::Vpn,
        ),
        (
            "Loopback Pseudo-Interface 1",
            AdapterKind::Loopback,
            Role::Ignored,
        ),
        ("Zscaler Loopback", AdapterKind::Loopback, Role::Ignored),
    ];
    for (name, kind, expected) in cases {
        let mut a = ethernet(name);
        a.kind = kind;
        assert_eq!(role(&a), expected, "{name}");
    }
}

#[test]
fn the_description_counts_as_well_as_the_name() {
    let mut a = adapter(3, "Ethernet 3", AdapterKind::Ethernet, "10.0.0.2", 24);
    a.description = "Fortinet SSL VPN Virtual Ethernet Adapter".to_owned();
    assert_eq!(role(&a), Role::Vpn);
}

#[test]
fn the_lab_private_access_client_puts_its_names_and_addresses_in_the_company_network() {
    let lab = PrivateAccess::adapter();
    let mut client = adapter(
        20,
        lab.description,
        AdapterKind::Ethernet,
        &lab.address.to_string(),
        lab.route.1,
    );
    client.dns_suffix = Some(lab.dns_suffix.to_owned());
    let mut snapshot = home();
    snapshot.adapters[1].subnets = vec![(IpAddr::V4(PrivateAccess::LAN_ADDRESS), 24)];
    snapshot.adapters.push(client);
    snapshot
        .routes
        .push(route(&lab.route.0.to_string(), lab.route.1, 20, 6));
    let signals = signals(snapshot);

    let (name, answer) = PrivateAccess::answers().remove(0);
    let host = Host::parse_normalised(name).expect("a host name");
    // The name is under the client's suffix: that is the first signal that matches.
    assert_eq!(
        classify_with_signals(answer, Some(&host), &signals),
        AddressClass::Company(CompanySignal::DnsSuffix)
    );
    // The bare address is a company address through the client's adapter.
    assert_eq!(
        classify_with_signals(answer, None, &signals),
        AddressClass::Company(CompanySignal::PrivateAccessCgnat)
    );
    // A printer on the physical LAN is still a private network address.
    assert!(matches!(
        classify_with_signals(IpAddr::V4(PrivateAccess::LAN_ADDRESS), None, &signals),
        AddressClass::Local {
            category: LocalCategory::Private,
            ..
        }
    ));
    // Behind the client the same name answers to the company toggle in the guard itself.
    let guard =
        NetPolicy::new(Arc::new(LocalAccess::default())).with_company_signals(Arc::new(signals));
    assert_eq!(
        guard.company_signal(Some(&host), SocketAddr::new(answer, 443)),
        Some(CompanySignal::DnsSuffix)
    );
}

#[test]
fn without_the_client_the_same_answer_is_plain_private() {
    let signals = signals(home());
    let answer = IpAddr::V4(PrivateAccess::ADDRESS);
    let host = Host::parse_normalised(PrivateAccess::NAME).expect("a host name");
    assert!(matches!(
        classify_with_signals(answer, Some(&host), &signals),
        AddressClass::Local { .. }
    ));
}

#[test]
fn a_vpn_route_puts_a_private_address_in_the_company_network() {
    let signals = signals(with_vpn());
    assert_eq!(
        classify_with_signals(ip("10.20.3.4"), None, &signals),
        AddressClass::Company(CompanySignal::VpnRoute)
    );
    assert!(matches!(
        classify_with_signals(ip("192.168.1.9"), None, &signals),
        AddressClass::Local { .. }
    ));
}

#[test]
fn a_network_that_cannot_be_read_keeps_every_address_out_of_the_company_network() {
    let signals = HostSignals::new(FakeNetwork::failing("no adapters"), None);
    assert_eq!(signals.physical_lan(ip("10.20.3.4")), LanReading::Unread);
    assert!(!signals.in_dns_namespace("a.corp.example"));
    assert!(!signals.private_access_client());
    assert!(!signals.vpn_routed(ip("10.20.3.4")));
    assert!(!signals.pac_direct("a.corp.example"));
    assert_eq!(signals.found(), CompanyFound::default());
    assert!(matches!(
        classify_with_signals(ip("10.20.3.4"), None, &signals),
        AddressClass::Local { .. }
    ));
}

#[test]
fn found_lists_the_client_and_the_suffixes() {
    let mut snapshot = with_vpn();
    snapshot.adapters.push(adapter(
        30,
        "Tailscale",
        AdapterKind::Other,
        "100.100.1.1",
        32,
    ));
    let found = signals(snapshot).found();
    assert!(found.private_access_client);
    assert_eq!(found.dns_suffixes, ["corp.contoso.example", "fritz.box"]);
}

#[test]
fn a_read_is_reused_for_the_snapshot_lifetime_and_renewed_after_it() {
    let network = FakeNetwork::new(home());
    let signals =
        HostSignals::new(network.clone(), None).with_snapshot_ttl(Duration::from_secs(3600));
    for _ in 0..5 {
        signals.physical_lan(ip("192.168.1.2"));
        signals.in_dns_namespace("a.example");
    }
    assert_eq!(network.reads(), 1);

    let network = FakeNetwork::new(home());
    let signals = HostSignals::new(network.clone(), None).with_snapshot_ttl(Duration::ZERO);
    assert!(!signals.vpn_routed(ip("10.20.3.4")));
    network.set(with_vpn());
    assert!(
        signals.vpn_routed(ip("10.20.3.4")),
        "the next read sees the VPN"
    );
    assert_eq!(network.reads(), 2);
}

#[test]
fn the_default_signals_have_a_snapshot_lifetime_of_seconds() {
    let network = FakeNetwork::new(home());
    let signals = HostSignals::new(network.clone(), None);
    signals.physical_lan(ip("192.168.1.2"));
    signals.physical_lan(ip("192.168.1.3"));
    assert_eq!(network.reads(), 1);
    assert_eq!(signals.ttl, Duration::from_secs(10));
}

#[cfg(not(windows))]
#[test]
fn a_platform_without_readers_has_an_empty_category() {
    let signals = HostSignals::system();
    assert_eq!(signals.physical_lan(ip("192.168.1.2")), LanReading::Unread);
    assert!(!signals.in_dns_namespace("a.corp.example"));
    assert!(!signals.pac_direct("a.corp.example"));
}

#[cfg(windows)]
#[test]
fn windows_reads_its_own_network() {
    let signals = HostSignals::system();
    // Loopback is no LAN, so a machine that reads its adapters answers a definite "no" for it.
    assert_eq!(signals.physical_lan(ip("127.0.0.1")), LanReading::OffLan);
}

mod proxy_script {
    use super::*;

    fn script(pac: impl Fn(&str) -> Vec<Hop> + Send + Sync + 'static) -> Arc<FakeOs> {
        let os = FakeOs::new(ProxyConfig {
            pac_url: Some("http://wpad.corp.example/proxy.pac".to_owned()),
            ..ProxyConfig::default()
        });
        os.set_pac(move |query| Ok(pac(&query.url)));
        os
    }

    fn corporate(url: &str) -> Vec<Hop> {
        if url.contains("intranet.corp.example") {
            vec![Hop::Direct]
        } else {
            vec![
                Hop::Proxy(ProxyAddr::new("proxy.corp.example", 8080)),
                Hop::Direct,
            ]
        }
    }

    fn inline() -> Spawn {
        Arc::new(|job| job())
    }

    fn reader(os: &Arc<FakeOs>, timing: PacTiming) -> PacReader {
        PacReader::with_spawn(os.clone(), timing, inline())
    }

    #[test]
    fn a_name_the_script_sends_direct_is_found_on_the_second_question() {
        let os = script(corporate);
        let pac = reader(&os, PacTiming::default());
        assert!(
            !pac.direct("intranet.corp.example"),
            "the first question starts the evaluation"
        );
        assert!(pac.direct("intranet.corp.example"));
        assert!(
            pac.direct("Intranet.Corp.Example."),
            "case and a trailing dot do not matter"
        );
        assert!(!pac.direct("www.github.com"));
        assert!(!pac.direct("www.github.com"));
    }

    #[test]
    fn a_script_that_sends_everything_direct_names_no_internal_host() {
        let os = script(|_| vec![Hop::Direct]);
        let pac = reader(&os, PacTiming::default());
        pac.direct("intranet.corp.example");
        assert!(!pac.direct("intranet.corp.example"));
    }

    #[test]
    fn a_script_that_proxies_the_name_does_not_make_it_internal() {
        let os = script(|_| {
            vec![
                Hop::Proxy(ProxyAddr::new("proxy.corp.example", 8080)),
                Hop::Direct,
            ]
        });
        let pac = reader(&os, PacTiming::default());
        pac.direct("intranet.corp.example");
        assert!(
            !pac.direct("intranet.corp.example"),
            "only the first hop counts"
        );
    }

    #[test]
    fn answers_are_kept_and_asked_again_after_their_lifetime() {
        let os = script(corporate);
        let pac = reader(&os, PacTiming::default());
        pac.direct("intranet.corp.example");
        let asked = os.pac_calls();
        for _ in 0..5 {
            assert!(pac.direct("intranet.corp.example"));
        }
        assert_eq!(os.pac_calls(), asked, "a kept answer asks nothing");

        let expiring = reader(
            &os,
            PacTiming {
                ttl: Duration::ZERO,
                ..PacTiming::default()
            },
        );
        expiring.direct("intranet.corp.example");
        let before = os.pac_calls();
        expiring.direct("intranet.corp.example");
        assert!(os.pac_calls() > before, "an expired answer is asked again");
    }

    #[test]
    fn no_script_means_no_signal_and_no_repeated_lookups() {
        let os = FakeOs::new(ProxyConfig::default());
        let pac = reader(&os, PacTiming::default());
        for _ in 0..3 {
            assert!(!pac.direct("intranet.corp.example"));
        }
        assert_eq!(os.pac_calls(), 0);
        assert_eq!(os.config_calls(), 1, "the failure is kept for a while");
    }

    #[test]
    fn unreadable_settings_mean_no_signal() {
        let os = script(corporate);
        os.fail_config("registry unreadable");
        let pac = reader(&os, PacTiming::default());
        pac.direct("intranet.corp.example");
        assert!(!pac.direct("intranet.corp.example"));
    }

    #[test]
    fn a_script_that_cannot_be_fetched_means_no_signal_and_is_not_asked_for_every_request() {
        let os = FakeOs::new(ProxyConfig {
            auto_detect: true,
            ..ProxyConfig::default()
        });
        os.set_pac(|_| Err(PacError::Unavailable("no WPAD".to_owned())));
        let pac = reader(&os, PacTiming::default());
        for _ in 0..4 {
            assert!(!pac.direct("intranet.corp.example"));
        }
        assert_eq!(os.pac_calls(), 1);
    }

    #[test]
    fn a_script_that_fails_for_the_name_only_means_no_signal() {
        let os = script(corporate);
        os.set_pac(|query| {
            if query.url.contains("intranet.corp.example") {
                Err(PacError::Failed("script error".to_owned()))
            } else {
                Ok(vec![Hop::Proxy(ProxyAddr::new("proxy.corp.example", 8080))])
            }
        });
        let pac = reader(&os, PacTiming::default());
        pac.direct("intranet.corp.example");
        assert!(!pac.direct("intranet.corp.example"));
    }

    #[test]
    fn addresses_and_empty_names_are_not_asked_about() {
        let os = script(corporate);
        let pac = reader(&os, PacTiming::default());
        for name in ["10.1.2.3", "::1", "", "."] {
            assert!(!pac.direct(name));
        }
        assert_eq!(os.config_calls(), 0);
    }

    type Job = Box<dyn FnOnce() + Send>;

    /// A spawner that only queues, so a test decides when the evaluations run.
    fn queued() -> (Spawn, Arc<Mutex<Vec<Job>>>) {
        let jobs: Arc<Mutex<Vec<Job>>> = Arc::default();
        let queue = jobs.clone();
        let spawn: Spawn = Arc::new(move |job| queue.lock().unwrap().push(job));
        (spawn, jobs)
    }

    #[test]
    fn a_name_being_evaluated_is_not_evaluated_twice() {
        let os = script(corporate);
        let (spawn, jobs) = queued();
        let pac = PacReader::with_spawn(os.clone(), PacTiming::default(), spawn);
        for _ in 0..3 {
            assert!(!pac.direct("intranet.corp.example"));
        }
        assert_eq!(jobs.lock().unwrap().len(), 1);
        let job = jobs.lock().unwrap().pop().unwrap();
        job();
        assert!(pac.direct("intranet.corp.example"));
    }

    #[test]
    fn only_a_few_names_are_evaluated_at_once() {
        let os = script(corporate);
        let (spawn, jobs) = queued();
        let pac = PacReader::with_spawn(os, PacTiming::default(), spawn);
        for n in 0..10 {
            pac.direct(&format!("host{n}.corp.example"));
        }
        assert_eq!(jobs.lock().unwrap().len(), 4);
        // A finished evaluation frees its place.
        let job = jobs.lock().unwrap().pop().unwrap();
        job();
        pac.direct("later.corp.example");
        assert_eq!(jobs.lock().unwrap().len(), 4);
    }

    #[test]
    fn a_job_that_never_ran_frees_its_place() {
        let os = script(corporate);
        let dropped = Arc::new(AtomicUsize::new(0));
        let counter = dropped.clone();
        let spawn: Spawn = Arc::new(move |job| {
            counter.fetch_add(1, Ordering::SeqCst);
            drop(job);
        });
        let pac = PacReader::with_spawn(os, PacTiming::default(), spawn);
        for _ in 0..3 {
            pac.direct("intranet.corp.example");
        }
        assert_eq!(
            dropped.load(Ordering::SeqCst),
            3,
            "each question may start one again"
        );
    }

    #[test]
    fn the_answers_kept_are_bounded() {
        let os = script(corporate);
        let pac = reader(&os, PacTiming::default());
        for n in 0..(MAX_NAMES_FOR_TEST + 5) {
            pac.direct(&format!("host{n}.corp.example"));
        }
        // The newest answer survives the clearing that kept the table bounded.
        assert!(!pac.direct("host0.intranet.corp.example"));
        pac.direct("x.intranet.corp.example");
        assert!(pac.direct("x.intranet.corp.example"));
    }

    const MAX_NAMES_FOR_TEST: usize = 1024;

    #[tokio::test]
    async fn the_default_reader_evaluates_on_a_thread_of_its_own() {
        let os = script(corporate);
        let pac = PacReader::new(os, PacTiming::default());
        pac.direct("intranet.corp.example");
        puddle_test_kit::eventually(
            "the background evaluation finished",
            Duration::from_secs(10),
            || pac.direct("intranet.corp.example"),
        )
        .await;
    }

    #[test]
    fn host_signals_answer_the_script_signal_from_the_reader() {
        let os = script(corporate);
        let signals = HostSignals::new(FakeNetwork::new(home()), None)
            .with_pac(reader(&os, PacTiming::default()));
        assert!(!signals.pac_direct("intranet.corp.example"));
        assert!(signals.pac_direct("intranet.corp.example"));
        let guard = classify_with_signals(
            ip("10.20.3.4"),
            Some(&Host::parse_normalised("intranet.corp.example").expect("a host")),
            &signals,
        );
        assert_eq!(guard, AddressClass::Company(CompanySignal::PacDirect));
        let without = HostSignals::new(FakeNetwork::new(home()), None);
        assert!(!without.pac_direct("intranet.corp.example"));
    }
}

#[test]
fn the_shipped_options_carry_the_machines_signals_and_the_defaults_carry_none() {
    assert!(crate::HostOptions::default().company_signals.is_none());
    let shipped = crate::HostOptions::system();
    let signals = shipped
        .company_signals
        .expect("the shipped host reads signals");
    // A machine that cannot read its network answers "unread", never a plain "no" for the LAN.
    assert_ne!(signals.physical_lan(ip("127.0.0.1")), LanReading::OnLan);
}

#[test]
fn the_signals_name_themselves_in_debug_output_without_dumping_the_network() {
    let text = format!("{:?}", signals(with_vpn()));
    assert!(text.starts_with("HostSignals"), "{text}");
    assert!(!text.contains("Contoso"), "{text}");
}
