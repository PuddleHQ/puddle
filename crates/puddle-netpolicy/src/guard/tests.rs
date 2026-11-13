// SPDX-License-Identifier: GPL-3.0-or-later
use std::net::IpAddr;

use super::*;
use crate::{EndpointKind, Registration, normalise_host};

fn workspace() -> WorkspaceId {
    WorkspaceId::new("box").unwrap()
}

fn sa(s: &str) -> SocketAddr {
    s.parse().unwrap()
}

struct Lan;
impl HostAddrs for Lan {
    fn is_own(&self, ip: IpAddr) -> bool {
        ip == "192.168.1.10".parse::<IpAddr>().unwrap()
    }
}

fn guard(access: LocalAccess) -> (NetPolicy, Registration) {
    let g = NetPolicy::new(Arc::new(access)).with_host_addrs(Arc::new(Lan));
    let api = g
        .endpoints()
        .register(sa("127.0.0.1:7070"), EndpointKind::Api);
    (g, api)
}

fn all_on() -> LocalAccess {
    LocalCategory::ALL
        .into_iter()
        .fold(LocalAccess::NONE, |a, c| a.with_toggle(c, true))
}

#[test]
fn toggle_off_blocks_naming_the_category() {
    let (g, _api) = guard(LocalAccess::NONE);
    for (addr, c) in [
        ("127.0.0.1:80", LocalCategory::Loopback),
        ("10.1.2.3:80", LocalCategory::Private),
        ("169.254.1.1:80", LocalCategory::LinkLocal),
        ("169.254.169.254:80", LocalCategory::Metadata),
        ("224.0.0.1:80", LocalCategory::Special),
    ] {
        assert_eq!(
            g.check_address(&workspace(), sa(addr)),
            AddressVerdict::Block(BlockReason::LocalToggle(c)),
            "{addr}"
        );
    }
    assert_eq!(
        g.check_address(&workspace(), sa("8.8.8.8:80")),
        AddressVerdict::Allow
    );
}

#[test]
fn toggle_on_permits_exact_allows_only_unless_wildcards_reach_local() {
    let (g, _api) = guard(all_on());
    assert_eq!(
        g.check_address(&workspace(), sa("10.1.2.3:80")),
        AddressVerdict::ExactOnly(LocalCategory::Private)
    );
    let (g, _api) = guard(all_on().with_wildcards_reach_local(true));
    assert_eq!(
        g.check_address(&workspace(), sa("10.1.2.3:80")),
        AddressVerdict::Allow
    );
    // The wildcard setting never replaces a toggle.
    let (g, _api) = guard(LocalAccess::NONE.with_wildcards_reach_local(true));
    assert_eq!(
        g.check_address(&workspace(), sa("10.1.2.3:80")),
        AddressVerdict::Block(BlockReason::LocalToggle(LocalCategory::Private))
    );
}

#[test]
fn puddle_endpoints_are_their_own_class_and_ignore_every_toggle() {
    let (g, _api) = guard(all_on().with_wildcards_reach_local(true));
    for addr in [
        "127.0.0.1:7070",
        "[::1]:7070",
        "192.168.1.10:7070",
        "0.0.0.0:7070",
    ] {
        assert_eq!(
            g.classify(sa(addr)),
            AddressClass::PuddleEndpoint(EndpointKind::Api),
            "{addr}"
        );
        assert_eq!(
            g.check_address(&workspace(), sa(addr)),
            AddressVerdict::Block(BlockReason::PuddleEndpoint),
            "{addr}"
        );
    }
    assert_eq!(
        g.check_address(&workspace(), sa("127.0.0.1:7071")),
        AddressVerdict::Allow
    );
    assert_eq!(
        g.check_address(&workspace(), sa("192.168.1.11:7070")),
        AddressVerdict::Allow
    );
}

#[test]
fn the_name_stage_blocks_literals_and_category_names_before_the_rules() {
    let target = |h: &str| normalise_host(h).unwrap();
    let (off, _a) = guard(LocalAccess::NONE);
    let check = |g: &NetPolicy, h: &str, port| g.check_target(&workspace(), &target(h), port);
    assert_eq!(
        check(&off, "10.0.0.1", 80),
        Some(BlockReason::LocalToggle(LocalCategory::Private))
    );
    assert_eq!(
        check(&off, "localhost", 80),
        Some(BlockReason::LocalToggle(LocalCategory::Loopback))
    );
    assert_eq!(
        check(&off, "metadata.google.internal", 80),
        Some(BlockReason::LocalToggle(LocalCategory::Metadata))
    );
    assert_eq!(check(&off, "example.com", 80), None);
    assert_eq!(check(&off, "8.8.8.8", 80), None);
    let (on, _b) = guard(all_on());
    // Toggle on: the rules decide (an exact allow or an approval).
    assert_eq!(check(&on, "10.0.0.1", 80), None);
    assert_eq!(check(&on, "localhost", 80), None);
    assert_eq!(check(&on, "metadata", 80), None);
    // Puddle's own endpoints, by literal or by `localhost`, whatever the toggles.
    assert_eq!(
        check(&on, "127.0.0.1", 7070),
        Some(BlockReason::PuddleEndpoint)
    );
    assert_eq!(
        check(&on, "localhost", 7070),
        Some(BlockReason::PuddleEndpoint)
    );
    assert_eq!(
        check(&on, "foo.localhost", 7070),
        Some(BlockReason::PuddleEndpoint)
    );
    assert_eq!(check(&on, "metadata", 7070), None);
    assert_eq!(
        check(&off, "localhost", 7070),
        Some(BlockReason::PuddleEndpoint)
    );
}

#[test]
fn toggles_are_read_per_workspace_per_call() {
    let per = |s: &WorkspaceId| {
        LocalAccess::NONE.with_toggle(LocalCategory::Loopback, s.as_str() == "dev")
    };
    let g = NetPolicy::new(Arc::new(per));
    let dev = WorkspaceId::new("dev").unwrap();
    assert_eq!(
        g.check_address(&dev, sa("127.0.0.1:3000")),
        AddressVerdict::ExactOnly(LocalCategory::Loopback)
    );
    assert_eq!(
        g.check_address(&workspace(), sa("127.0.0.1:3000")),
        AddressVerdict::Block(BlockReason::LocalToggle(LocalCategory::Loopback))
    );
    assert!(format!("{g:?}").starts_with("NetPolicy"));
}

#[test]
fn unreadable_settings_block_local_destinations_with_their_own_reason() {
    let g = NetPolicy::new(Arc::new(LocalAccess::UNREADABLE)).with_host_addrs(Arc::new(Lan));
    let reason = Some(BlockReason::SettingsUnreadable);
    for addr in ["127.0.0.1:3000", "10.0.0.5:80", "169.254.169.254:80"] {
        assert_eq!(
            g.check_address(&workspace(), sa(addr)),
            AddressVerdict::Block(BlockReason::SettingsUnreadable),
            "{addr}"
        );
    }
    let ip = |s: &str| normalise_host(s).unwrap();
    assert_eq!(g.check_target(&workspace(), &ip("10.0.0.5"), 80), reason);
    assert_eq!(g.check_target(&workspace(), &ip("localhost"), 80), reason);
    assert_eq!(g.check_target(&workspace(), &ip("127.0.0.1"), 80), reason);
    // Public addresses and Puddle's own endpoints are unaffected.
    assert_eq!(
        g.check_address(&workspace(), sa("93.184.216.34:443")),
        AddressVerdict::Allow
    );
    assert_eq!(g.check_target(&workspace(), &ip("example.com"), 443), None);
}

#[test]
fn the_unreadable_settings_message_says_so() {
    let host = Host::parse_normalised("nas.example").unwrap();
    let msg = block_message(
        &host,
        &workspace(),
        &[BlockReason::PuddleEndpoint, BlockReason::SettingsUnreadable],
    );
    assert!(msg.contains("could not read its settings"), "{msg}");
    assert!(msg.contains("Settings page"), "{msg}");
    assert!(!msg.contains("toggle is off"), "{msg}");
}

#[test]
fn block_messages_name_what_would_change_them() {
    let host = Host::parse_normalised("nas.example").unwrap();
    let one = block_message(
        &host,
        &workspace(),
        &[BlockReason::LocalToggle(LocalCategory::Private)],
    );
    assert_eq!(
        one,
        "nas.example is local/private network, and that toggle is off. To allow it, turn on 'private' (local/private network) globally or for workspace box; it then still needs an allow rule or an approval. Approving it alone does not change this"
    );
    let two = block_message(
        &host,
        &workspace(),
        &[
            BlockReason::LocalToggle(LocalCategory::Private),
            BlockReason::PuddleEndpoint,
            BlockReason::LocalToggle(LocalCategory::Loopback),
            BlockReason::LocalToggle(LocalCategory::Private),
        ],
    );
    assert!(
        two.contains("turn on 'loopback' (host loopback) or 'private' (local/private network)"),
        "{two}"
    );
    assert!(
        block_message(&host, &workspace(), &[BlockReason::PuddleEndpoint])
            .contains("Puddle's own endpoints")
    );
    assert_eq!(
        block_message(&host, &workspace(), &[BlockReason::SshUnsupported]),
        "SSH is not supported yet; no rule or setting allows it"
    );
    assert_eq!(
        block_message(&host, &workspace(), &[BlockReason::UdpUnsupported]),
        format!(
            "UDP to {host} is not supported yet; only TCP and DNS leave a workspace, and no rule or setting changes this"
        )
    );
    assert!(
        block_message(&host, &workspace(), &[BlockReason::LocalAddress]).contains("local address")
    );
    assert!(block_message(&host, &workspace(), &[]).contains("local address"));
}

#[test]
fn ssh_to_a_git_host_names_the_https_form_of_the_remote_and_any_other_host_does_not() {
    for (host, https) in [
        ("github.com", "https://github.com/OWNER/REPO.git"),
        ("ssh.github.com", "https://github.com/OWNER/REPO.git"),
        ("gitlab.com", "https://gitlab.com/GROUP/PROJECT.git"),
        ("bitbucket.org", "https://bitbucket.org/WORKSPACE/REPO.git"),
        (
            "ssh.dev.azure.com",
            "https://dev.azure.com/ORGANIZATION/PROJECT/_git/REPO",
        ),
        (
            "vs-ssh.visualstudio.com",
            "https://dev.azure.com/ORGANIZATION/PROJECT/_git/REPO",
        ),
    ] {
        let host = Host::parse_normalised(host).unwrap();
        assert_eq!(git_https_remote(&host), Some(https));
        assert_eq!(
            block_message(&host, &workspace(), &[BlockReason::SshUnsupported]),
            format!("SSH is not supported yet, use HTTPS: the remote becomes {https}")
        );
    }
    for other in [
        "git.example.com",
        "gitlab.com.evil.example",
        "notgithub.com",
        "dev.azure.com",
        "203.0.113.7",
        "2001:db8::1",
    ] {
        let host = Host::parse_normalised(other).unwrap();
        assert_eq!(git_https_remote(&host), None, "{other}");
        assert_eq!(
            block_message(&host, &workspace(), &[BlockReason::SshUnsupported]),
            "SSH is not supported yet; no rule or setting allows it",
            "{other}"
        );
    }
}

#[test]
fn classify_recognises_the_endpoints_of_a_registry_it_was_given() {
    let registry = PuddleEndpoints::new();
    let _api = registry.register(sa("127.0.0.1:7071"), EndpointKind::Api);
    let g = NetPolicy::new(Arc::new(LocalAccess::NONE)).with_endpoints(registry);
    assert_eq!(
        g.classify(sa("127.0.0.1:7071")),
        AddressClass::PuddleEndpoint(EndpointKind::Api)
    );
    assert_eq!(g.endpoints().list().len(), 1);
}
// The company-network category (the host's signals decide; the toggle decides what follows).

use crate::StaticSignals;

fn company_guard(access: LocalAccess) -> NetPolicy {
    let signals = StaticSignals::new()
        .with_dns_suffix("corp.example")
        .with_private_access_client()
        .with_vpn_route("10.8.0.0".parse().unwrap(), 16)
        .with_lan_subnet("192.168.1.0".parse().unwrap(), 24);
    NetPolicy::new(Arc::new(access))
        .with_host_addrs(Arc::new(Lan))
        .with_company_signals(Arc::new(signals))
}

fn named(s: &str) -> Host {
    Host::parse_normalised(s).unwrap()
}

#[test]
fn company_toggle_is_off_by_default_and_blocks_with_the_signal() {
    let g = company_guard(all_on_but_company());
    let w = workspace();
    let host = named("nexus.corp.example");
    assert_eq!(
        g.check_address_named(&w, Some(&host), sa("10.1.2.3:443")),
        AddressVerdict::Block(BlockReason::CompanyNetwork(CompanySignal::DnsSuffix))
    );
    assert_eq!(
        g.check_address_named(&w, None, sa("100.64.3.7:443")),
        AddressVerdict::Block(BlockReason::CompanyNetwork(
            CompanySignal::PrivateAccessCgnat
        ))
    );
    assert_eq!(
        g.check_address_named(&w, None, sa("10.8.1.1:443")),
        AddressVerdict::Block(BlockReason::CompanyNetwork(CompanySignal::VpnRoute))
    );
}

fn all_on_but_company() -> LocalAccess {
    all_on().with_toggle(LocalCategory::CompanyNetwork, false)
}

#[test]
fn company_toggle_on_permits_exact_allows_only_and_never_the_private_toggle() {
    let w = workspace();
    // Company on, private off: the company address is reachable, a plain private one is not.
    let on = LocalAccess::NONE.with_toggle(LocalCategory::CompanyNetwork, true);
    let g = company_guard(on);
    assert_eq!(
        g.check_address(&w, sa("100.64.3.7:443")),
        AddressVerdict::ExactOnly(LocalCategory::CompanyNetwork)
    );
    assert_eq!(
        g.check_address(&w, sa("172.16.0.9:443")),
        AddressVerdict::Block(BlockReason::LocalToggle(LocalCategory::Private))
    );
    // Wildcards reach it only when that setting is on.
    let g = company_guard(on.with_wildcards_reach_local(true));
    assert_eq!(
        g.check_address(&w, sa("100.64.3.7:443")),
        AddressVerdict::Allow
    );
    // Company off, private on: the company address still needs the company toggle.
    let g = company_guard(LocalAccess::NONE.with_toggle(LocalCategory::Private, true));
    assert!(matches!(
        g.check_address(&w, sa("100.64.3.7:443")),
        AddressVerdict::Block(BlockReason::CompanyNetwork(_))
    ));
}

#[test]
fn what_the_company_toggle_never_reaches() {
    let g = company_guard(LocalAccess::NONE.with_toggle(LocalCategory::CompanyNetwork, true));
    let w = workspace();
    let host = named("nexus.corp.example");
    for (addr, category) in [
        ("127.0.0.1:80", LocalCategory::Loopback),
        ("169.254.1.1:80", LocalCategory::LinkLocal),
        ("169.254.169.254:80", LocalCategory::Metadata),
        ("224.0.0.1:80", LocalCategory::Special),
    ] {
        assert_eq!(
            g.check_address_named(&w, Some(&host), sa(addr)),
            AddressVerdict::Block(BlockReason::LocalToggle(category)),
            "{addr}"
        );
    }
    // A public address stays public whatever its name says.
    assert_eq!(
        g.check_address_named(&w, Some(&host), sa("8.8.8.8:443")),
        AddressVerdict::Allow
    );
    // The physical LAN's own subnet stays local/private even under a company suffix.
    assert_eq!(
        g.check_address_named(&w, Some(&host), sa("192.168.1.40:443")),
        AddressVerdict::Block(BlockReason::LocalToggle(LocalCategory::Private))
    );
    // Puddle's own listener.
    let (g2, _api) = guard(LocalAccess::NONE);
    assert_eq!(
        g2.check_address(&w, sa("127.0.0.1:7070")),
        AddressVerdict::Block(BlockReason::PuddleEndpoint)
    );
}

#[test]
fn without_signals_the_category_is_empty() {
    let (g, _api) = guard(LocalAccess::NONE);
    let host = named("nexus.corp.example");
    assert_eq!(
        g.check_address_named(&workspace(), Some(&host), sa("100.64.3.7:443")),
        AddressVerdict::Block(BlockReason::LocalToggle(LocalCategory::Private))
    );
    assert_eq!(g.company_signal(Some(&host), sa("10.1.2.3:443")), None);
}

#[test]
fn unreadable_settings_say_so_for_a_company_address_too() {
    let g = company_guard(LocalAccess::UNREADABLE);
    assert_eq!(
        g.check_address(&workspace(), sa("100.64.3.7:443")),
        AddressVerdict::Block(BlockReason::SettingsUnreadable)
    );
}

#[test]
fn a_literal_company_address_is_blocked_before_the_rules() {
    let g = company_guard(LocalAccess::NONE);
    let literal = normalise_host("100.64.3.7").unwrap();
    assert_eq!(
        g.check_target(&workspace(), &literal, 443),
        Some(BlockReason::CompanyNetwork(
            CompanySignal::PrivateAccessCgnat
        ))
    );
    // A name is only known by its address, after the rules.
    let name = normalise_host("nexus.corp.example").unwrap();
    assert_eq!(g.check_target(&workspace(), &name, 443), None);
}

#[test]
fn the_audit_signal_is_reported_for_allowed_and_refused_addresses() {
    let on = company_guard(LocalAccess::NONE.with_toggle(LocalCategory::CompanyNetwork, true));
    let host = named("nexus.corp.example");
    assert_eq!(
        on.company_signal(Some(&host), sa("10.1.2.3:443")),
        Some(CompanySignal::DnsSuffix)
    );
    assert_eq!(on.company_signal(Some(&host), sa("8.8.8.8:443")), None);
}

#[test]
fn the_company_refusal_names_the_switch_and_does_not_promise_access() {
    let host = named("nexus.corp.example");
    let ip: IpAddr = "100.64.3.7".parse().unwrap();
    assert_eq!(
        company_network_message(&host, Some(ip), Some(CompanySignal::VpnRoute)),
        "nexus.corp.example (100.64.3.7) is on your company network (reached through your VPN). Turn on 'Company network' for this workspace to be able to allow it"
    );
    // No signal, no address: the brackets are left out.
    assert_eq!(
        company_network_message(&host, None, None),
        "nexus.corp.example is on your company network. Turn on 'Company network' for this workspace to be able to allow it"
    );
    // An IP literal is not repeated in brackets.
    let literal = named("100.64.3.7");
    assert!(
        company_network_message(&literal, Some(ip), None)
            .starts_with("100.64.3.7 is on your company network.")
    );
    // block_message gives the same text for the block reason.
    assert_eq!(
        block_message(
            &host,
            &workspace(),
            &[BlockReason::CompanyNetwork(CompanySignal::DnsSuffix)]
        ),
        company_network_message(&host, None, Some(CompanySignal::DnsSuffix))
    );
}

#[test]
fn a_blocked_public_name_is_refused_at_the_name_stage_whatever_the_toggles() {
    let target = |h: &str| normalise_host(h).unwrap();
    let (g, _api) = guard(all_on());
    let check = |g: &NetPolicy, h: &str| g.check_target(&workspace(), &target(h), 443);
    assert_eq!(check(&g, "puddle.example.org"), None, "not blocked yet");
    let front = g
        .endpoints()
        .block_name("puddle.example.org", false, EndpointKind::ExternalFront);
    let apps = g
        .endpoints()
        .block_name("ws.example.net", true, EndpointKind::ExternalFront);
    for name in [
        "puddle.example.org",
        "PUDDLE.example.org",
        "ws.example.net",
        "box.ws.example.net",
    ] {
        assert_eq!(check(&g, name), Some(BlockReason::PuddleEndpoint), "{name}");
    }
    for name in [
        "example.org",
        "sub.puddle.example.org",
        "evilws.example.net",
        "example.com",
    ] {
        assert_eq!(check(&g, name), None, "{name}");
    }
    drop(front);
    assert_eq!(
        check(&g, "puddle.example.org"),
        None,
        "dropping lifts the block"
    );
    assert_eq!(
        check(&g, "ws.example.net"),
        Some(BlockReason::PuddleEndpoint)
    );
    drop(apps);
    assert_eq!(check(&g, "ws.example.net"), None);
}

#[test]
fn a_guest_cannot_reach_the_front_or_the_network_listener_with_every_toggle_on() {
    let target = |h: &str| normalise_host(h).unwrap();
    let (g, _api) = guard(all_on());
    let dials = ["192.168.1.10:443", "127.0.0.1:443", "192.168.1.10:8443"];
    // The control: nothing registered, so a toggle that is on lets the rules decide.
    for dial in dials {
        assert!(
            !matches!(
                g.check_address(&workspace(), sa(dial)),
                AddressVerdict::Block(_)
            ),
            "{dial} unregistered"
        );
    }
    let _front = g
        .endpoints()
        .register(sa("0.0.0.0:443"), EndpointKind::ExternalFront);
    let _listener = g
        .endpoints()
        .register(sa("192.168.1.10:8443"), EndpointKind::RemoteApi);
    for dial in dials {
        assert_eq!(
            g.check_address(&workspace(), sa(dial)),
            AddressVerdict::Block(BlockReason::PuddleEndpoint),
            "{dial}"
        );
    }
    // By literal at the name stage too, before the rules are asked.
    assert_eq!(
        g.check_target(&workspace(), &target("192.168.1.10"), 8443),
        Some(BlockReason::PuddleEndpoint)
    );
    // Another host's port 443 is not ours.
    assert_eq!(
        g.check_address(&workspace(), sa("8.8.8.8:443")),
        AddressVerdict::Allow
    );
}

// Which addresses a certificate distribution point may be fetched from without a rule.

#[test]
fn a_distribution_point_may_be_on_a_private_or_company_address_with_every_toggle_off() {
    let g = company_guard(LocalAccess::NONE);
    let pki = named("pki.corp.example");
    // a private address, and one the DNS suffix puts in the company network, and a CGNAT one
    assert!(g.pki_address(Some(&pki), sa("10.1.2.3:80")));
    assert!(g.pki_address(Some(&pki), sa("172.16.0.9:80")));
    assert!(g.pki_address(None, sa("100.64.3.7:80")));
    assert!(g.pki_address(Some(&pki), sa("10.8.1.1:80")));
    // the physical LAN stays private, and private qualifies
    assert!(g.pki_address(Some(&pki), sa("192.168.1.77:80")));
}

#[test]
fn a_distribution_point_never_reaches_loopback_link_local_metadata_public_or_puddle() {
    let (g, _api) = guard(LocalAccess::NONE);
    let pki = named("pki.corp.example");
    for addr in [
        "127.0.0.1:80",
        "[::1]:80",
        "169.254.1.1:80",
        "169.254.169.254:80",
        "0.0.0.0:80",
        "192.0.2.1:80",
        "8.8.8.8:80",
        "[2001:4860:4860::8888]:80",
        // Puddle's own listener, even though it is on loopback
        "127.0.0.1:7070",
    ] {
        assert!(!g.pki_address(Some(&pki), sa(addr)), "{addr}");
    }
}

#[test]
fn the_azure_devops_message_names_the_host_the_port_and_the_https_way() {
    let text = azure_devops_ssh_message();
    assert!(text.contains("ssh.dev.azure.com:22"), "{text}");
    assert!(text.contains("HTTPS remotes work"), "{text}");
    assert!(!text.contains("rule"), "{text}");
}
