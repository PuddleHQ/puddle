// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use proptest::prelude::*;

fn reason(text: &str) -> String {
    Host::parse_normalised(text)
        .unwrap_err()
        .reason()
        .to_owned()
}

#[test]
fn accepts_normalised_names() {
    for ok in [
        "example.com",
        "a.b-c.example",
        "localhost",
        "xn--bcher-kva.example",
        "1a.com",
    ] {
        assert!(Host::parse_normalised(ok).is_ok(), "{ok}");
    }
}

#[test]
fn refuses_unnormalised_names() {
    let cases = [
        ("", "must not be empty"),
        ("Example.com", "may only contain"),
        ("example.com.", "empty label"),
        (".example.com", "empty label"),
        ("a..b", "empty label"),
        ("-a.com", "may only contain"),
        ("a-.com", "may only contain"),
        ("a_b.com", "may only contain"),
        ("bücher.example", "may only contain"),
        ("*.example.com", "may only contain"),
        ("01.2.3.4", "all-numeric"),
        ("example.123", "all-numeric"),
    ];
    for (input, want) in cases {
        assert!(reason(input).contains(want), "{input:?}: {}", reason(input));
    }
    let long_label = format!("{}.com", "a".repeat(64));
    assert!(reason(&long_label).contains("longer than 63"));
    let long_name = vec!["a".repeat(63); 4].join(".");
    assert!(reason(&long_name).contains("at most 253"));
    assert_eq!(
        Host::parse_normalised("Example.com").unwrap_err().what(),
        "host name"
    );
}

#[test]
fn ip_literals_must_be_canonical() {
    for ok in ["10.0.0.1", "::1", "::ffff:10.0.0.1"] {
        assert!(
            matches!(Host::parse_normalised(ok), Ok(Host::Ip(_))),
            "{ok}"
        );
    }
    for bad in [
        "0:0:0:0:0:0:0:1",
        "2001:DB8::1",
        "[::1]",
        "::1%eth0",
        "fe80::1:",
    ] {
        let err = Host::parse_normalised(bad).unwrap_err();
        assert_eq!(err.what(), "ip literal", "{bad}");
    }
}

#[test]
fn host_accessors_and_display() {
    let name = Host::parse_normalised("a.example.com").unwrap();
    assert_eq!(name.to_string(), "a.example.com");
    let labels: Vec<_> = name.name().unwrap().labels().collect();
    assert_eq!(labels, ["a", "example", "com"]);
    assert_eq!(name.name().unwrap().as_str(), "a.example.com");
    let ip = Host::parse_normalised("2001:db8::1").unwrap();
    assert!(ip.name().is_none());
    assert_eq!(ip.to_string(), "2001:db8::1");
}

proptest! {
    #[test]
    fn accepted_hosts_display_as_their_input(s in "[a-z0-9.:-]{1,40}") {
        if let Ok(host) = Host::parse_normalised(&s) {
            prop_assert_eq!(host.to_string(), s);
        }
    }

    #[test]
    fn ips_round_trip(ip in any::<IpAddr>()) {
        let text = ip.to_string();
        prop_assert_eq!(Host::parse_normalised(&text), Ok(Host::Ip(ip)));
    }

    #[test]
    fn arbitrary_input_never_panics(s in ".{0,300}") {
        let _ = Host::parse_normalised(&s);
    }
}
