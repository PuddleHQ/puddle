// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

fn dest(host: &str) -> Destination {
    Destination::new(Scheme::Https, host, 443)
}

#[test]
fn a_proxy_list_says_which_entries_it_could_not_use() {
    let (rules, problems) =
        ProxyRules::parse_with_problems("http=good:8080;https=bad:port;ftp=f:21;socks=s:1080");
    assert_eq!(
        rules.for_scheme(Scheme::Http),
        Some(&ProxyAddr::new("good", 8080))
    );
    assert_eq!(rules.for_scheme(Scheme::Https), None);
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(
        problems[0].contains("https=bad:port") && problems[0].contains("bad port"),
        "{problems:?}"
    );
    // A bare entry that is not an address is a problem too; the first good one still wins.
    let (rules, problems) = ProxyRules::parse_with_problems("a:1 b:x c:3");
    assert_eq!(
        rules.for_scheme(Scheme::Https),
        Some(&ProxyAddr::new("a", 1))
    );
    assert_eq!(problems.len(), 1, "{problems:?}");
}

#[test]
fn a_list_with_nothing_usable_in_it_says_so_once() {
    for text in [
        "socks=s:1080",
        "ftp=f:21;socks=s:1080",
        ";",
        "socks5://s:1080",
    ] {
        let (rules, problems) = ProxyRules::parse_with_problems(text);
        assert!(rules.is_empty(), "{text}");
        assert_eq!(problems.len(), 1, "{text}: {problems:?}");
    }
    for quiet in ["", "   ", "http=a:1", "a:1;socks=s:1"] {
        assert!(
            ProxyRules::parse_with_problems(quiet).1.is_empty(),
            "{quiet:?}"
        );
    }
}

#[test]
fn an_entry_in_a_problem_text_carries_no_user_info() {
    let (_, problems) = ProxyRules::parse_with_problems("http=user:hunter2@bad host:80x");
    let shown = format!("{problems:?}");
    assert!(!shown.contains("hunter2"), "{shown}");
    let (_, problems) = ProxyRules::parse_with_problems("user:hunter2@p:port");
    assert!(!format!("{problems:?}").contains("hunter2"), "{problems:?}");
}

#[test]
fn pac_answers_become_ordered_hops_and_count_what_is_skipped() {
    let a = parse_pac_answer("PROXY 127.0.0.1:1; proxy p.corp:3128;HTTP q:8080 ; DIRECT");
    assert_eq!(
        a.hops,
        vec![
            Hop::Proxy(ProxyAddr::new("127.0.0.1", 1)),
            Hop::Proxy(ProxyAddr::new("p.corp", 3128)),
            Hop::Proxy(ProxyAddr::new("q", 8080)),
            Hop::Direct
        ]
    );
    assert_eq!(a.skipped, 0);
    let b = parse_pac_answer("SOCKS s:1; HTTPS t:443; PROXY; BOGUS x; DIRECT");
    assert_eq!(b.hops, vec![Hop::Direct]);
    assert_eq!(b.skipped, 4);
    assert_eq!(parse_pac_answer(""), PacAnswer::default());
}

#[test]
fn proxy_rules_per_scheme() {
    let split = ProxyRules::parse("http=a:1;https=b:2;ftp=c:3;socks=d:4");
    assert_eq!(
        split.for_scheme(Scheme::Https),
        Some(&ProxyAddr::new("b", 2))
    );
    assert_eq!(
        split.for_scheme(Scheme::Http),
        Some(&ProxyAddr::new("a", 1))
    );
    let one = ProxyRules::parse("a:1");
    assert_eq!(one.for_scheme(Scheme::Https), Some(&ProxyAddr::new("a", 1)));
    let https_only = ProxyRules::parse("https=b:2");
    assert_eq!(https_only.for_scheme(Scheme::Http), None);
    assert_eq!(ProxyRules::parse("ftp=a:1").for_scheme(Scheme::Https), None);
    assert_eq!(ProxyRules::parse("").for_scheme(Scheme::Http), None);
    // WinHTTP's machine list is space separated and may carry a default plus overrides.
    let mixed = ProxyRules::parse("x:1 https=y:2");
    assert_eq!(
        mixed.for_scheme(Scheme::Https),
        Some(&ProxyAddr::new("y", 2))
    );
    assert_eq!(
        mixed.for_scheme(Scheme::Http),
        Some(&ProxyAddr::new("x", 1))
    );
}

#[test]
fn bypass_list_matches_like_proxy_override() {
    let list = BypassList::parse(
        "*.corp.test;<local>;exact.example;10.*;https://only.tls;web:8080;.dom.test",
    );
    for (host, expected) in [
        ("nexus.corp.test", true),
        ("corp.test", false),
        ("intranet", true),
        ("EXACT.example.", true),
        ("sub.exact.example", false),
        ("10.1.2.3", true),
        ("110.1.2.3", false),
        ("only.tls", true),
        ("dom.test", true),
        ("a.dom.test", true),
        ("evildom.test", false),
        ("::2", false),
        ("github.com", false),
    ] {
        assert_eq!(list.matches(&dest(host)), expected, "{host}");
    }
    assert!(!list.matches(&Destination::new(Scheme::Http, "only.tls", 80)));
    assert!(list.matches(&Destination::new(Scheme::Http, "web", 8080)));
    assert!(!list.matches(&Destination::new(Scheme::Http, "web.x", 80)));
    assert!(!BypassList::parse("").matches(&dest("x")));
    assert!(!BypassList::parse("<-loopback>;ftp://x;:80;web:zz").matches(&dest("web")));
}

#[test]
fn no_proxy_forms() {
    let list = BypassList::parse_no_proxy(
        "corp.test, .dotted.test,*.star.test,exact.example,host.test:8443,10.0.0.0/8,fd00::/8, ,bad:x,/8,1.2.3.4/33",
    );
    for (host, port, expected) in [
        ("corp.test", 443, true),
        ("a.corp.test", 443, true),
        ("evilcorp.test", 443, false),
        ("a.dotted.test", 443, true),
        ("dotted.test", 443, true),
        ("a.star.test", 443, true),
        ("x.exact.example", 443, true),
        ("host.test", 8443, true),
        ("host.test", 443, false),
        ("10.9.9.9", 443, true),
        ("11.0.0.1", 443, false),
        ("fd12::1", 443, true),
        ("fe80::1", 443, false),
        ("github.com", 443, false),
    ] {
        assert_eq!(
            list.matches(&Destination::new(Scheme::Https, host, port)),
            expected,
            "{host}:{port}"
        );
    }
    assert!(BypassList::parse_no_proxy("*").matches(&dest("anything")));
    assert!(!BypassList::parse_no_proxy("").matches(&dest("anything")));
    let all = BypassList::parse_no_proxy("0.0.0.0/0,1.2.3.4/32");
    assert!(all.matches(&dest("200.1.1.1")));
    assert!(!BypassList::parse_no_proxy("1.2.3.4/32").matches(&dest("1.2.3.5")));
    assert!(!BypassList::parse_no_proxy("10.0.0.0/8").matches(&dest("fd00::1")));
}

#[test]
fn glob_handles_stars_anywhere() {
    assert!(glob_match("*", ""));
    assert!(glob_match("a*b*c", "aXXbYYc"));
    assert!(glob_match("*.x", "a.b.x"));
    assert!(!glob_match("*.x", "x"));
    assert!(!glob_match("a*b", "acd"));
    assert!(glob_match("a**", "a"));
}

mod props {
    use super::super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn parsers_never_panic(text in "\\PC{0,80}") {
            let _ = parse_pac_answer(&text);
            let _ = ProxyRules::parse(&text);
            let list = BypassList::parse(&text);
            let _ = list.matches(&Destination::new(Scheme::Https, &text, 443));
            let _ = ProxyAddr::parse(&text);
        }

        #[test]
        fn a_literal_pattern_matches_only_itself(host in "[a-z0-9.-]{0,19}[a-z0-9-]", other in "[a-z0-9.-]{0,19}[a-z0-9-]") {
            prop_assume!(!host.starts_with('.') && !host.starts_with('<'));
            let list = BypassList::parse(&host);
            let own = Destination::new(Scheme::Https, &host, 443);
            prop_assert_eq!(list.matches(&Destination::new(Scheme::Https, &other, 443)), own.host() == other);
            prop_assert!(list.matches(&own));
        }

        #[test]
        fn pac_hops_never_exceed_entries(text in "[A-Za-z0-9:;. ]{0,60}") {
            let a = parse_pac_answer(&text);
            prop_assert!(a.hops.len() + a.skipped <= text.split(';').count());
        }
    }
}
