// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use proptest::prelude::*;

#[test]
fn a_name_is_lower_cased_and_loses_its_trailing_dot() {
    assert_eq!(
        DnsName::parse("Api.GitHub.com.").unwrap().as_str(),
        "api.github.com"
    );
    assert_eq!(
        DnsName::parse("a_b.example").unwrap().to_string(),
        "a_b.example"
    );
}

#[test]
fn what_is_not_a_plain_dns_name_is_refused() {
    for bad in [
        "",
        ".",
        "*.github.com",
        ".github.com",
        "git hub.com",
        "-a.com",
        "a-.com",
        "a..com",
        "1.2.3.4",
        "1.2.3.4.5",
        "140.82.112.3",
        "::1",
        "[::1]",
        "github.com:443",
        "bücher.example",
        &"a".repeat(64),
        &format!("{}.com", "a.".repeat(130)),
    ] {
        assert!(DnsName::parse(bad).is_none(), "{bad:?} accepted");
    }
}

#[test]
fn the_length_limits_sit_exactly_at_253_and_63_bytes() {
    let label = |n: usize| "a".repeat(n);
    // 63 + 63 + 63 + 61 bytes and three dots is exactly 253.
    let longest = format!("{}.{}.{}.{}", label(63), label(63), label(63), label(61));
    assert_eq!(longest.len(), 253);
    assert!(DnsName::parse(&longest).is_some());
    let too_long = format!("{}.{}.{}.{}", label(63), label(63), label(63), label(62));
    assert_eq!(too_long.len(), 254);
    assert!(DnsName::parse(&too_long).is_none());
    // A trailing dot is not counted.
    assert!(DnsName::parse(&format!("{longest}.")).is_some());

    assert!(DnsName::parse(&format!("{}.com", label(63))).is_some());
    assert!(DnsName::parse(&format!("{}.com", label(64))).is_none());
    assert!(DnsName::parse(&format!("a.{}", label(64))).is_none());
}

#[test]
fn a_label_holds_only_letters_digits_hyphens_and_underscores() {
    // The neighbours of each accepted range in ASCII order.
    for good in [
        "a-b.com", "a_b.com", "a0.com", "a9.com", "az.com", "_a.com", "0a.com", "9a.com", "za.com",
    ] {
        assert!(DnsName::parse(good).is_some(), "{good:?} refused");
    }
    for bad in [
        "a`b.com",
        "a{b.com",
        "a/b.com",
        "a:b.com",
        "a@b.com",
        "a[b.com",
        "a\\b.com",
        "a^b.com",
        "a b.com",
        "a+b.com",
        "a,b.com",
        "a=b.com",
        "a%b.com",
        "a\0b.com",
        "a\u{7f}b.com",
        "a\tb.com",
    ] {
        assert!(DnsName::parse(bad).is_none(), "{bad:?} accepted");
    }
    // Upper case is folded to lower case before the check.
    assert_eq!(DnsName::parse("AZ-09_.Com").unwrap().as_str(), "az-09_.com");
    // The last label alone decides the all-digit rule.
    assert!(DnsName::parse("1.2.3.com").is_some());
    assert!(DnsName::parse("a.1").is_none());
    assert!(DnsName::parse("1a").is_some());
}

proptest! {
    #[test]
    fn parsing_never_panics_and_is_idempotent(input in "\\PC{0,80}") {
        if let Some(name) = DnsName::parse(&input) {
            prop_assert_eq!(DnsName::parse(name.as_str()), Some(name));
        }
    }
}
