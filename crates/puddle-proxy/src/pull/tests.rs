// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;

fn head(headers: &[&str]) -> Head {
    Head {
        method: "CONNECT".into(),
        uri: "registry.test:443".into(),
        version: "HTTP/1.1".into(),
        headers: headers.iter().map(|h| (*h).to_owned()).collect(),
    }
}

fn token() -> PullToken {
    PullToken("ab".repeat(TOKEN_BYTES))
}

#[test]
fn a_failed_pull_is_marked_unless_something_already_decided_it() {
    let event = || {
        ConnectionEvent::puddle(
            Host::parse_normalised("registry.test").unwrap(),
            443,
            ConnectionDecision::Allow,
            ConnectionReason::PuddleRequest,
        )
    };
    for (status, outcome, code) in [
        ("504 Gateway Timeout", ConnectionOutcome::Timeout, "timeout"),
        (
            "502 Bad Gateway",
            ConnectionOutcome::ConnectFailed,
            "unreachable",
        ),
        (
            "502 Bad Gateway",
            ConnectionOutcome::UpstreamRefused,
            "unreachable",
        ),
    ] {
        let mut failed = event();
        note_failure(&mut failed, &Refusal::new(status, "no").failed(outcome));
        assert_eq!(failed.reason, ConnectionReason::PuddleRequestFailed(code));
        assert_eq!(failed.outcome, Some(outcome));
    }
    let mut blocked = event();
    blocked.decision = ConnectionDecision::Blocked;
    let before = blocked.reason;
    note_failure(&mut blocked, &Refusal::new("403 Forbidden", "no"));
    assert_eq!(blocked.reason, before);
}

#[test]
fn a_token_is_64_hex_characters_and_fresh_each_time() {
    let a = PullToken::generate().unwrap();
    let b = PullToken::generate().unwrap();
    assert_eq!(a.expose().len(), 64);
    assert!(
        a.expose()
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    );
    assert_ne!(a.expose(), b.expose());
}

#[test]
fn the_proxy_debug_names_its_address_and_hides_the_token() {
    let endpoints = PuddleEndpoints::new();
    let proxy = PullProxy::bind(&endpoints).unwrap();
    let shown = format!("{proxy:?}");
    assert!(shown.starts_with("PullProxy {"), "{shown}");
    assert!(shown.contains(&proxy.local_addr().to_string()), "{shown}");
    assert!(!shown.contains(proxy.token().expose()), "{shown}");
}

#[test]
fn debug_never_shows_the_token() {
    let t = token();
    assert_eq!(format!("{t:?}"), "PullToken(<redacted>)");
    let url = ProxyUrl {
        url: format!("http://puddle:{}@127.0.0.1:9", t.expose()),
        addr: "127.0.0.1:9".parse().unwrap(),
    };
    assert_eq!(url.to_string(), "http://puddle:<redacted>@127.0.0.1:9");
    assert_eq!(
        format!("{url:?}"),
        "ProxyUrl(http://puddle:<redacted>@127.0.0.1:9)"
    );
    assert!(url.expose().contains(t.expose()));
}

#[test]
fn only_the_exact_basic_credentials_are_accepted() {
    let t = token();
    let good = t.basic_credentials();
    let ok = |headers: &[&str]| authorised(&head(headers), &good);
    assert!(ok(&[&format!("Proxy-Authorization: Basic {good}")]));
    assert!(ok(&[&format!("proxy-authorization:   basic {good}  ")]));
    assert!(ok(&[
        "Host: x",
        &format!("PROXY-AUTHORIZATION: BASIC {good}")
    ]));

    assert!(!ok(&[]), "missing");
    assert!(!ok(&["Authorization: Basic x"]), "wrong header");
    let wrong = BASE64.encode(format!("puddle:{}", "cd".repeat(TOKEN_BYTES)));
    assert!(
        !ok(&[&format!("Proxy-Authorization: Basic {wrong}")]),
        "wrong token"
    );
    let user = BASE64.encode(format!("admin:{}", t.expose()));
    assert!(
        !ok(&[&format!("Proxy-Authorization: Basic {user}")]),
        "wrong user"
    );
    assert!(
        !ok(&[&format!("Proxy-Authorization: Bearer {good}")]),
        "scheme"
    );
    assert!(
        !ok(&[&format!("Proxy-Authorization: Basic{good}")]),
        "no space"
    );
    assert!(!ok(&["Proxy-Authorization: Basic"]), "empty");
    assert!(
        !ok(&[&format!("Proxy-Authorization: Basic {}", t.expose())]),
        "raw token, not base64"
    );
    let both = format!("Proxy-Authorization: Basic {good}");
    assert!(!ok(&[&both, &both]), "duplicated header");
    assert!(
        !ok(&[&both, &format!("Proxy-Authorization: Basic {wrong}")]),
        "a second, wrong header"
    );
    assert!(
        !ok(&[&format!("Proxy-Authorization: Basic {good}x")]),
        "suffix"
    );
}

#[test]
fn pulls_reach_public_private_and_loopback_but_not_metadata_link_local_or_puddle() {
    let endpoints = PuddleEndpoints::new();
    let _api = endpoints.register("127.0.0.1:7070".parse().unwrap(), EndpointKind::Api);
    let access = default_pull_access();
    let guard = NetPolicy::new(Arc::new(access)).with_endpoints(endpoints);
    let v = |a: &str| verdict(&guard, access, None, a.parse().unwrap());
    assert_eq!(v("140.82.121.4:443"), Ok(()));
    assert_eq!(v("10.1.2.3:443"), Ok(()));
    assert_eq!(v("[fd00::1]:443"), Ok(()));
    assert_eq!(v("127.0.0.1:5000"), Ok(()));
    assert_eq!(v("127.0.0.1:7070"), Err(BlockReason::PuddleEndpoint));
    assert_eq!(
        v("169.254.169.254:80"),
        Err(BlockReason::LocalToggle(LocalCategory::Metadata))
    );
    assert_eq!(
        v("168.63.129.16:80"),
        Err(BlockReason::LocalToggle(LocalCategory::Metadata))
    );
    assert_eq!(
        v("169.254.1.1:80"),
        Err(BlockReason::LocalToggle(LocalCategory::LinkLocal))
    );
    assert_eq!(
        v("224.0.0.1:80"),
        Err(BlockReason::LocalToggle(LocalCategory::Special))
    );
    let none = LocalAccess::NONE;
    let strict = NetPolicy::new(Arc::new(none));
    assert_eq!(
        verdict(&strict, none, None, "10.1.2.3:443".parse().unwrap()),
        Err(BlockReason::LocalToggle(LocalCategory::Private))
    );
}

#[test]
fn a_company_registry_is_reachable_by_default_and_refused_when_its_toggle_is_off() {
    let signals = Arc::new(puddle_netpolicy::StaticSignals::new().with_private_access_client());
    let guard_for = |access| NetPolicy::new(Arc::new(access)).with_company_signals(signals.clone());
    let access = default_pull_access();
    let guard = guard_for(access);
    assert_eq!(
        verdict(&guard, access, None, "100.64.3.7:443".parse().unwrap()),
        Ok(())
    );
    // Switched off, the pull is refused with the company reason and the header names it.
    let off = access.with_toggle(LocalCategory::CompanyNetwork, false);
    let guard = guard_for(off);
    let reason = verdict(&guard, off, None, "100.64.3.7:443".parse().unwrap()).unwrap_err();
    assert!(matches!(reason, BlockReason::CompanyNetwork(_)));
    let host = Host::parse_normalised("registry.corp.example").unwrap();
    let mut event = ConnectionEvent::puddle(
        host.clone(),
        443,
        ConnectionDecision::Allow,
        ConnectionReason::PuddleRequest,
    );
    let refusal = blocked(&host, 443, &[reason], &mut event);
    assert_eq!(
        refusal.message,
        "registry.corp.example is a company network address; image pulls reach it only while the 'company_network' switch is on in Settings"
    );
    assert!(
        refusal
            .headers
            .iter()
            .any(|(n, v)| *n == "x-puddle-blocked" && v == "toggle:company_network")
    );
}

#[test]
fn a_refusal_a_switch_can_lift_names_the_switch_and_the_others_say_never() {
    let host = Host::parse_normalised("registry.test").unwrap();
    let text = |reason: BlockReason| {
        let mut event = ConnectionEvent::puddle(
            host.clone(),
            443,
            ConnectionDecision::Allow,
            ConnectionReason::PuddleRequest,
        );
        blocked(&host, 443, &[reason], &mut event).message
    };
    assert_eq!(
        text(BlockReason::LocalToggle(LocalCategory::Private)),
        format!(
            "registry.test is a {} address; image pulls reach it only while the 'private' switch is on in Settings",
            LocalCategory::Private.describe()
        )
    );
    assert_eq!(
        text(BlockReason::PuddleEndpoint),
        "registry.test is one of Puddle's own endpoints; image pulls never go there"
    );
    assert_eq!(
        text(BlockReason::LocalAddress),
        "registry.test is an address image pulls may not reach; image pulls never go there"
    );
}

#[test]
fn a_block_names_puddle_endpoints_first() {
    let host = Host::parse_normalised("localhost").unwrap();
    let mut event = ConnectionEvent::puddle(
        host.clone(),
        1,
        ConnectionDecision::Allow,
        ConnectionReason::PuddleRequest,
    );
    let r = blocked(
        &host,
        1,
        &[
            BlockReason::LocalToggle(LocalCategory::Special),
            BlockReason::PuddleEndpoint,
        ],
        &mut event,
    );
    assert_eq!(
        (event.decision, event.reason),
        (
            ConnectionDecision::Blocked,
            ConnectionReason::Blocked(BlockReason::PuddleEndpoint)
        )
    );
    assert_eq!(r.status, "403 Forbidden");
    assert!(
        r.headers
            .contains(&("x-puddle-blocked", "puddle_endpoint".to_owned()))
    );
    assert!(
        r.message.contains("Puddle's own endpoints"),
        "{}",
        r.message
    );
    let r = blocked(
        &host,
        1,
        &[BlockReason::LocalToggle(LocalCategory::Metadata)],
        &mut event,
    );
    assert!(r.message.contains("metadata"), "{}", r.message);
    let r = blocked(&host, 1, &[], &mut event);
    assert!(
        r.headers
            .contains(&("x-puddle-blocked", "local_address".to_owned()))
    );
}
