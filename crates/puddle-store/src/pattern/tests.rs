// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use proptest::prelude::*;

fn host(s: &str) -> Host {
    Host::parse_normalised(s).unwrap()
}

#[test]
fn r02_exact_rule_matches_identical_host_only() {
    let p = Pattern::parse("api.example.com").unwrap();
    assert_eq!(p.kind(), PatternKind::Exact);
    assert!(p.matches(&host("api.example.com")));
    assert!(!p.matches(&host("x.api.example.com")));
    assert!(!p.matches(&host("example.com")));
    let ip = Pattern::parse("2001:db8::1").unwrap();
    assert!(ip.matches(&host("2001:db8::1")));
    assert!(!ip.matches(&host("2001:db8::2")));
}

#[test]
fn r03_suffix_matches_any_depth_but_not_apex_or_ip() {
    let p = Pattern::parse(".example.com").unwrap();
    assert_eq!(p.kind(), PatternKind::Suffix);
    assert!(p.matches(&host("a.example.com")));
    assert!(p.matches(&host("a.b.c.example.com")));
    assert!(!p.matches(&host("example.com")));
    assert!(!p.matches(&host("badexample.com")));
    assert!(!p.matches(&host("example.com.evil.net")));
    assert!(!p.matches(&host("93.184.216.34")));
    assert_eq!(Pattern::parse("*.example.com").unwrap(), p);
    assert_eq!(p.to_string(), ".example.com");
}

#[test]
fn r03_suffix_of_ip_literal_is_refused() {
    assert_eq!(Pattern::parse(".10.0.0.1"), Err(PatternError::SuffixOfIp));
    assert_eq!(Pattern::parse("*.::1"), Err(PatternError::SuffixOfIp));
}

#[test]
fn r04_public_suffix_patterns_are_refused() {
    for bad in [".com", "*.co.uk", ".github.io", ".localhost", ".uk"] {
        assert!(
            matches!(Pattern::parse(bad), Err(PatternError::PublicSuffix(_))),
            "{bad}"
        );
    }
    for ok in [
        ".example.com",
        ".example.co.uk",
        ".me.github.io",
        ".corp.internal",
    ] {
        assert!(Pattern::parse(ok).is_ok(), "{ok}");
    }
}

#[test]
fn invalid_hosts_are_refused() {
    assert!(matches!(
        Pattern::parse("Example.com"),
        Err(PatternError::Host(_))
    ));
    assert!(matches!(
        Pattern::parse("*.*.example.com"),
        Err(PatternError::Host(_))
    ));
    assert!(matches!(Pattern::parse(""), Err(PatternError::Host(_))));
}

#[test]
fn suffix_covering_checks_the_host() {
    let h = host("a.b.example.com");
    assert_eq!(
        Pattern::suffix_covering(&h, "example.com")
            .unwrap()
            .to_string(),
        ".example.com"
    );
    assert!(Pattern::suffix_covering(&h, "*.b.example.com").is_ok());
    assert!(matches!(
        Pattern::suffix_covering(&h, "other.com"),
        Err(PatternError::NotASuffixOf { .. })
    ));
    assert!(matches!(
        Pattern::suffix_covering(&h, ".a.b.example.com"),
        Err(PatternError::NotASuffixOf { .. })
    ));
    assert!(matches!(
        Pattern::suffix_covering(&h, "com"),
        Err(PatternError::PublicSuffix(_))
    ));
    assert_eq!(
        Pattern::suffix_covering(&host("10.0.0.1"), "example.com"),
        Err(PatternError::SuffixOfIp)
    );
}

#[test]
fn specificity_orders_exact_then_longer_suffix() {
    let exact = Pattern::parse("a.b.example.com").unwrap();
    let long = Pattern::parse(".b.example.com").unwrap();
    let short = Pattern::parse(".example.com").unwrap();
    assert!(exact.specificity() > long.specificity());
    assert!(long.specificity() > short.specificity());
}

#[test]
fn r18_registrable_domain_groups() {
    assert_eq!(
        registrable_domain(&host("a.b.example.co.uk")),
        "example.co.uk"
    );
    assert_eq!(registrable_domain(&host("example.com")), "example.com");
    assert_eq!(registrable_domain(&host("com")), "com");
    assert_eq!(registrable_domain(&host("10.1.2.3")), "10.1.2.3");
}

proptest! {
    #[test]
    fn suffix_matches_exactly_the_names_below_it(
        base in "[a-z]{1,8}\\.example",
        head in proptest::collection::vec("[a-z0-9]{1,6}", 0..4),
    ) {
        let pattern = Pattern::parse(&format!(".{base}")).unwrap();
        let name = if head.is_empty() { base.clone() } else { format!("{}.{base}", head.join(".")) };
        prop_assert_eq!(pattern.matches(&host(&name)), !head.is_empty());
    }

    #[test]
    fn parse_never_panics(s in ".{0,80}") {
        let _ = Pattern::parse(&s);
    }
}
