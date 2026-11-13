// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn proxy_addr_reads_the_forms_windows_and_pac_use() {
    for (text, host, port) in [
        ("proxy.corp:3128", "proxy.corp", 3128),
        ("PROXY.Corp:8080", "proxy.corp", 8080),
        ("proxy.corp", "proxy.corp", 80),
        ("http://proxy.corp:8080/", "proxy.corp", 8080),
        ("http://proxy.corp", "proxy.corp", 80),
        ("[::1]:9000", "::1", 9000),
        ("[fe80::1]", "fe80::1", 80),
        ("fe80::1", "fe80::1", 80),
        ("  127.0.0.1:9000 ", "127.0.0.1", 9000),
    ] {
        let addr = ProxyAddr::parse(text).unwrap();
        assert_eq!((addr.host(), addr.port()), (host, port), "{text}");
    }
}

#[test]
fn proxy_addr_drops_credentials_and_refuses_what_it_cannot_speak() {
    let addr = ProxyAddr::parse("http://user:secret@proxy.corp:3128").unwrap();
    assert_eq!(addr, ProxyAddr::new("proxy.corp", 3128));
    assert!(!format!("{addr:?}").contains("secret"));
    assert_eq!(ProxyAddr::parse(""), Err(ParseError::Empty));
    assert_eq!(ProxyAddr::parse("http://"), Err(ParseError::Empty));
    assert_eq!(ProxyAddr::parse("host:0"), Err(ParseError::BadPort));
    assert_eq!(ProxyAddr::parse("host:99999"), Err(ParseError::BadPort));
    assert_eq!(ProxyAddr::parse("host:x"), Err(ParseError::BadPort));
    assert_eq!(ProxyAddr::parse(":80"), Err(ParseError::BadHost));
    assert_eq!(ProxyAddr::parse("[::1"), Err(ParseError::BadHost));
    assert_eq!(ProxyAddr::parse("[::1]x"), Err(ParseError::BadHost));
    assert_eq!(ProxyAddr::parse("a b:80"), Err(ParseError::BadHost));
    assert_eq!(
        ProxyAddr::parse("https://proxy:443"),
        Err(ParseError::UnsupportedScheme("https".into()))
    );
    assert_eq!(
        ProxyAddr::parse("socks5://p:1080"),
        Err(ParseError::UnsupportedScheme("socks5".into()))
    );
}

#[test]
fn display_round_trips_and_brackets_ipv6() {
    assert_eq!(ProxyAddr::new("p.corp", 1).to_string(), "p.corp:1");
    assert_eq!(ProxyAddr::new("::1", 2).to_string(), "[::1]:2");
    let route = Route::new(vec![
        Hop::Proxy(ProxyAddr::new("a", 1)),
        Hop::Proxy(ProxyAddr::new("b", 2)),
        Hop::Direct,
    ])
    .unwrap();
    assert_eq!(route.to_string(), "PROXY a:1; PROXY b:2; DIRECT");
}

#[test]
fn a_route_is_never_empty_and_demotion_never_shortens_it() {
    assert!(Route::new(vec![]).is_none());
    assert!(Route::direct().is_direct());
    let a = ProxyAddr::new("a", 1);
    let b = ProxyAddr::new("b", 2);
    let route = Route::new(vec![
        Hop::Proxy(a.clone()),
        Hop::Proxy(b.clone()),
        Hop::Direct,
    ])
    .unwrap();
    assert!(!route.is_direct());
    let demoted = route.demote(|p| *p == a);
    assert_eq!(
        demoted.hops(),
        &[Hop::Proxy(b.clone()), Hop::Direct, Hop::Proxy(a.clone())]
    );
    let all_bad = route.demote(|_| true);
    assert_eq!(all_bad.hops().len(), 3);
    assert_eq!(all_bad.hops().first(), Some(&Hop::Direct));
    assert_eq!(Route::via(b.clone()).demote(|_| true), Route::via(b));
}

#[test]
fn destination_normalises_and_builds_the_pac_url_like_a_browser() {
    let d = Destination::new(Scheme::Https, "GitHub.COM.", 443);
    assert_eq!(d.host(), "github.com");
    assert_eq!(d.pac_url(), "https://github.com/");
    assert_eq!(
        Destination::new(Scheme::Http, "x.test", 80).pac_url(),
        "http://x.test/"
    );
    assert_eq!(
        Destination::new(Scheme::Https, "x.test", 8443).pac_url(),
        "https://x.test:8443/"
    );
    assert_eq!(
        Destination::new(Scheme::Https, "[::2]", 443).pac_url(),
        "https://[::2]/"
    );
    assert_eq!(
        Destination::new(Scheme::Http, "x", 1).scheme(),
        Scheme::Http
    );
    assert_eq!(Destination::new(Scheme::Http, "x", 1).port(), 1);
}

#[test]
fn loopback_destinations_are_recognised() {
    for host in [
        "localhost",
        "LOCALHOST",
        "app.localhost",
        "127.0.0.1",
        "127.9.9.9",
        "::1",
        "0.0.0.0",
        "::",
        "::ffff:127.0.0.1",
    ] {
        assert!(
            Destination::new(Scheme::Https, host, 443).is_loopback(),
            "{host}"
        );
    }
    for host in [
        "github.com",
        "10.0.0.1",
        "localhost.example",
        "128.0.0.1",
        "::2",
    ] {
        assert!(
            !Destination::new(Scheme::Https, host, 443).is_loopback(),
            "{host}"
        );
    }
}
