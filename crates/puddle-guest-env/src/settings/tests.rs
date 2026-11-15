// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn defaults_are_the_agent_the_bridge_and_root() {
    let s = ProxySettings::default();
    assert_eq!(s.proxy.to_string(), "127.0.0.1:3128");
    assert_eq!(s.container_proxy.unwrap().to_string(), "172.17.0.1:3128");
    assert_eq!(s.home.as_str(), "/root");
    assert_eq!(s.no_proxy, []);
}

#[test]
fn accepted_entries_are_normalised() {
    for (input, out) in [
        ("Example.COM", "example.com"),
        (".corp.example", ".corp.example"),
        ("host-1", "host-1"),
        ("10.1.2.3", "10.1.2.3"),
        ("fd00::1", "fd00::1"),
        ("[fd00::1]", "fd00::1"),
        ("xn--bcher-kva.example", "xn--bcher-kva.example"),
    ] {
        assert_eq!(NoProxyEntry::new(input).unwrap().as_str(), out, "{input}");
        assert_eq!(NoProxyEntry::new(input).unwrap().to_string(), out);
    }
}

#[test]
fn refused_entries_say_why() {
    let long_label = format!("{}.example", "a".repeat(64));
    let long_name = ["a"; 128].join(".");
    for (input, reason) in [
        ("", "empty"),
        ("10.0.0.0/8", "CIDR"),
        ("*.corp", "wildcards"),
        ("host:8080", "ports"),
        (".", "empty"),
        ("a..b", "empty label"),
        ("bad_name", "letters"),
        ("b\u{fc}cher.example", "letters"),
        ("-a.example", "'-'"),
        ("a-.example", "'-'"),
        ("a b", "letters"),
        ("a,b", "letters"),
        ("a\"b", "letters"),
        (long_label.as_str(), "63"),
        (long_name.as_str(), "253"),
    ] {
        let err = NoProxyEntry::new(input).unwrap_err();
        let ConfigError::NoProxyEntry { entry, reason: r } = &err else {
            panic!("{err:?}")
        };
        assert_eq!(entry, input);
        assert!(r.contains(reason), "{input:?}: {err}");
    }
}

#[test]
fn java_and_apt_forms() {
    let e = |s| NoProxyEntry::new(s).unwrap();
    assert_eq!(
        e("corp.example").java_patterns(),
        ["corp.example", "*.corp.example"]
    );
    assert_eq!(e(".corp.example").java_patterns(), ["*.corp.example"]);
    assert_eq!(e("fd00::1").java_patterns(), ["[fd00::1]"]);
    assert_eq!(e("10.0.0.1").java_patterns(), ["10.0.0.1"]);
    assert_eq!(
        e("corp.example").apt_host().as_deref(),
        Some("corp.example")
    );
    assert_eq!(e("10.0.0.1").apt_host().as_deref(), Some("10.0.0.1"));
    assert_eq!(e(".corp.example").apt_host(), None);
    assert_eq!(e("fd00::1").apt_host(), None);
}
