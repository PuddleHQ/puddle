// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

fn host(text: &str) -> PublicHost {
    PublicHost::parse(text).unwrap()
}

#[test]
fn a_public_host_is_a_host_and_an_optional_port_in_one_normal_form() {
    for (text, authority, origin, port) in [
        (
            "puddle.example.org",
            "puddle.example.org",
            "https://puddle.example.org",
            443,
        ),
        (
            "Puddle.Example.ORG",
            "puddle.example.org",
            "https://puddle.example.org",
            443,
        ),
        (
            "puddle.example.org.",
            "puddle.example.org",
            "https://puddle.example.org",
            443,
        ),
        (
            "puddle.example.org:443",
            "puddle.example.org",
            "https://puddle.example.org",
            443,
        ),
        (
            "puddle.example.org:8443",
            "puddle.example.org:8443",
            "https://puddle.example.org:8443",
            8443,
        ),
        ("203.0.113.7", "203.0.113.7", "https://203.0.113.7", 443),
        (
            "[2001:db8::1]:9000",
            "[2001:db8::1]:9000",
            "https://[2001:db8::1]:9000",
            9000,
        ),
        ("[::1]", "[::1]", "https://[::1]", 443),
    ] {
        let parsed = host(text);
        assert_eq!(parsed.authority(), authority, "{text}");
        assert_eq!(parsed.origin(), origin, "{text}");
        assert_eq!(parsed.port(), port, "{text}");
        assert_eq!(parsed.to_string(), authority);
    }
}

#[test]
fn a_public_host_refuses_what_is_not_a_bare_authority() {
    for text in [
        "",
        "https://puddle.example.org",
        "puddle.example.org/",
        "puddle.example.org/path",
        "user@puddle.example.org",
        "puddle.example.org?x",
        "puddle.example.org#x",
        "a b.example.org",
        "puddle.example.org:",
        "puddle.example.org:0",
        "puddle.example.org:65536",
        "puddle.example.org:http",
        "a..example.org",
        "-a.example.org",
        "a_b.example.org",
        "2001:db8::1",
        "[2001:db8::1",
        "[puddle.example.org]",
        "[1.2.3.4]",
        "1.2.3.4.5",
    ] {
        let err = PublicHost::parse(text).unwrap_err();
        assert!(
            matches!(err, RemoteError::PublicHost { .. }),
            "{text:?}: {err}"
        );
        assert!(err.to_string().contains("write a host name"), "{err}");
    }
}

#[test]
fn the_base_domain_defaults_to_subdomains_of_the_first_public_host() {
    let remote = RemoteServing::new(
        vec![host("puddle.example.org"), host("alt.example.org")],
        None,
    )
    .unwrap();
    let names = remote.workspace_names();
    assert_eq!(names.base_domain(), "puddle.example.org");
    assert_eq!(names.host_for("box"), "box.puddle.example.org");
    assert_eq!(names.origin_for("box"), "https://box.puddle.example.org");
    assert_eq!(remote.public_hosts().len(), 2);
}

#[test]
fn a_set_base_domain_replaces_it_and_keeps_the_front_port() {
    let remote = RemoteServing::new(
        vec![host("puddle.example.org:8443")],
        Some("WS.Example.Net."),
    )
    .unwrap();
    let names = remote.workspace_names();
    assert_eq!(names.base_domain(), "ws.example.net");
    assert_eq!(names.host_for("box"), "box.ws.example.net:8443");
    assert_eq!(names.origin_for("box"), "https://box.ws.example.net:8443");
}

#[test]
fn a_host_header_maps_to_its_workspace_and_nothing_else_does() {
    let remote =
        RemoteServing::new(vec![host("puddle.example.org")], Some("ws.example.net")).unwrap();
    let names = remote.workspace_names();
    assert_eq!(
        names.workspace_of("box.ws.example.net").as_deref(),
        Some("box")
    );
    assert_eq!(
        names.workspace_of("BOX.ws.example.net").as_deref(),
        Some("box")
    );
    for other in [
        "ws.example.net",
        "a.b.ws.example.net",
        ".ws.example.net",
        "box.ws.example.net:8443",
        "boxws.example.net",
        "box.ws.example.net.evil.test",
        "puddle.example.org",
        "",
    ] {
        assert_eq!(names.workspace_of(other), None, "{other:?}");
    }
    let ported = RemoteServing::new(vec![host("puddle.example.org:8443")], None).unwrap();
    let names = ported.workspace_names();
    assert_eq!(
        names.workspace_of("box.puddle.example.org:8443").as_deref(),
        Some("box")
    );
    assert_eq!(names.workspace_of("box.puddle.example.org"), None);
}

#[test]
fn the_base_domain_covers_itself_and_what_is_below_it() {
    let remote =
        RemoteServing::new(vec![host("puddle.example.org")], Some("ws.example.net")).unwrap();
    let names = remote.workspace_names();
    for name in ["ws.example.net", "box.ws.example.net", "A.B.ws.example.net"] {
        assert!(names.covers(name), "{name}");
    }
    for name in ["example.net", "xws.example.net", "ws.example.net.evil.test"] {
        assert!(!names.covers(name), "{name}");
    }
}

#[test]
fn remote_serving_refuses_settings_that_cannot_work() {
    assert_eq!(
        RemoteServing::new(Vec::new(), None),
        Err(RemoteError::NoPublicHost)
    );
    assert!(matches!(
        RemoteServing::new(vec![host("203.0.113.7")], None),
        Err(RemoteError::NoSubdomains { .. })
    ));
    assert!(matches!(
        RemoteServing::new(vec![host("[::1]")], None),
        Err(RemoteError::NoSubdomains { .. })
    ));
    // An address UI is fine with a base domain of its own.
    assert!(RemoteServing::new(vec![host("203.0.113.7")], Some("ws.example.net")).is_ok());
    assert!(matches!(
        RemoteServing::new(vec![host("puddle.example.org")], Some("puddle.example.org")),
        Err(RemoteError::BaseIsPublicHost { .. })
    ));
    assert!(matches!(
        RemoteServing::new(vec![host("puddle.example.org")], Some("not a domain")),
        Err(RemoteError::BaseDomain { .. })
    ));
}

#[test]
fn the_tls_identity_shows_a_fingerprint_and_never_its_key() {
    let identity = TlsIdentity::new(vec![1, 2, 3], vec![9, 9, 9, 9]);
    assert_eq!(
        identity.fingerprint(),
        "039058c6f2c0cb492c533b0a4d14ef77cc0f78abccced5287d84a1a2011cfb81"
    );
    let shown = format!("{identity:?}");
    assert!(shown.contains("039058c6"), "{shown}");
    assert!(!shown.contains("9, 9"), "{shown}");
    assert_eq!(identity.cert_der(), [1, 2, 3]);
}

fn ip(text: &str) -> IpAddr {
    text.parse().unwrap()
}

#[test]
fn a_network_endpoint_needs_a_proxy_that_may_connect_and_a_named_interface() {
    let endpoint = |interface: &str, allowed: &[&str]| {
        NetworkEndpoint::new(ip(interface), 0, allowed.iter().map(|a| ip(a)).collect())
    };
    assert_eq!(endpoint("10.0.0.1", &[]), Err(RemoteError::NoAllowedProxy));
    let ok = endpoint("10.0.0.1", &["10.0.0.2"]).unwrap();
    assert_eq!(ok.interface(), ip("10.0.0.1"));
    assert_eq!(ok.allowed_proxies(), [ip("10.0.0.2")]);
    // Every interface is not "one chosen interface", also written as a mapped IPv4 address.
    for any in ["0.0.0.0", "::", "::ffff:0.0.0.0"] {
        assert_eq!(
            endpoint(any, &["10.0.0.2"]),
            Err(RemoteError::UnspecifiedInterface { interface: ip(any) }),
            "{any}"
        );
    }
    // A peer address nobody has would pass an at-least-one check and admit nobody.
    for any in ["0.0.0.0", "::"] {
        assert_eq!(
            endpoint("10.0.0.1", &["10.0.0.2", any]),
            Err(RemoteError::UnspecifiedProxy { proxy: ip(any) }),
            "{any}"
        );
    }
    assert!(endpoint("::1", &["::1"]).is_ok());
}

#[test]
fn a_public_host_one_label_below_the_base_domain_is_refused() {
    // With a base domain of its own.
    assert!(matches!(
        RemoteServing::new(
            vec![host("puddle.example.org"), host("box.ws.example.net")],
            Some("ws.example.net")
        ),
        Err(RemoteError::PublicHostIsWorkspaceName { host, .. }) if host == "box.ws.example.net"
    ));
    // Without one the UI's host is the base, so a second host below it clashes too.
    assert!(matches!(
        RemoteServing::new(
            vec![host("puddle.example.org"), host("x.puddle.example.org")],
            None
        ),
        Err(RemoteError::PublicHostIsWorkspaceName { .. })
    ));
    // Deeper than one label, a sibling, or another domain is not a workspace's name.
    assert!(
        RemoteServing::new(
            vec![
                host("puddle.example.org"),
                host("a.b.ws.example.net"),
                host("ws.example.net.evil.test"),
                host("other.example.org"),
            ],
            Some("ws.example.net")
        )
        .is_ok()
    );
}
