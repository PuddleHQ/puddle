// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn an_exact_name_and_a_wildcard_read_back_as_written() {
    let exact = HostPattern::parse_normalised("api.example.com").unwrap();
    assert!(!exact.is_below());
    assert_eq!(exact.name().as_str(), "api.example.com");
    assert_eq!(exact.to_string(), "api.example.com");

    let below = HostPattern::parse_normalised("*.example.com").unwrap();
    assert!(below.is_below());
    assert_eq!(below.name().as_str(), "example.com");
    assert_eq!(below.to_string(), "*.example.com");
    assert_ne!(exact, below);
}

#[test]
fn a_wildcard_needs_two_labels_after_the_star() {
    assert!(HostPattern::parse_normalised("*.com").is_err());
    assert!(HostPattern::parse_normalised("*.a.com").is_ok());
    // One label with no wildcard is a name.
    assert!(HostPattern::parse_normalised("localhost").is_ok());
}

#[test]
fn what_is_no_pattern_is_refused() {
    for bad in [
        "",
        "*.",
        "*",
        "*.*.example.com",
        "a*.example.com",
        "Example.com",
        "example.com.",
        ".example.com",
        "203.0.113.7",
        "2001:db8::1",
        "*.203.0.113.7",
        "user@example.com",
        "example.com/path",
        " example.com",
    ] {
        assert!(HostPattern::parse_normalised(bad).is_err(), "{bad:?}");
    }
}

#[test]
fn the_error_names_the_pattern() {
    let message = HostPattern::parse_normalised("*.com")
        .unwrap_err()
        .to_string();
    assert!(message.contains("*.com"), "{message}");
}
