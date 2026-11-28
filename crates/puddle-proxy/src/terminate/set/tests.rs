// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

fn host(text: &str) -> Host {
    Host::parse_normalised(text).unwrap()
}

#[test]
fn patterns_cover_exactly_what_they_say() {
    let set = TerminationSet::parse(["github.com", "*.visualstudio.com", "dev.azure.com"]).unwrap();
    for yes in [
        "github.com",
        "dev.azure.com",
        "a.visualstudio.com",
        "a.b.visualstudio.com",
    ] {
        assert!(set.contains(&host(yes)), "{yes}");
    }
    for no in [
        "visualstudio.com",
        "api.github.com",
        "evilgithub.com",
        "github.com.evil.example",
        "notvisualstudio.com",
        "azure.com",
        "140.82.112.3",
        "::1",
    ] {
        assert!(!set.contains(&host(no)), "{no}");
    }
}

#[test]
fn bad_patterns_are_refused() {
    for bad in [
        "",
        "140.82.112.3",
        "*.com",
        "*.",
        "*",
        "a b.example",
        "https://github.com",
    ] {
        assert!(TerminationSet::parse([bad]).is_err(), "{bad:?}");
    }
    assert_eq!(
        TerminationSet::parse(["*.com"]),
        Err(PatternError::TooWide("*.com".into()))
    );
}

#[test]
fn patterns_are_normalised() {
    let set = TerminationSet::parse(["GitHub.com", "*.VisualStudio.com", "github.com"]).unwrap();
    assert!(set.contains(&host("github.com")));
    assert!(set.contains(&host("org.visualstudio.com")));
    assert_eq!(set.exact.len(), 1);
    assert!(!set.is_empty());
    assert!(TerminationSet::new().is_empty());
    assert!(!TerminationSet::new().contains(&host("github.com")));
}

#[test]
fn a_checked_pattern_makes_the_same_set_as_its_text() {
    for text in ["github.com", "*.visualstudio.com"] {
        let pattern = HostPattern::parse_normalised(text).unwrap();
        let mut set = TerminationSet::new();
        set.insert_pattern(&pattern);
        assert_eq!(set, TerminationSet::parse([text]).unwrap(), "{text}");
    }
    let mut set = TerminationSet::new();
    set.insert_pattern(&HostPattern::parse_normalised("*.example.com").unwrap());
    assert!(set.contains(&host("a.example.com")));
    assert!(!set.contains(&host("example.com")));
}
