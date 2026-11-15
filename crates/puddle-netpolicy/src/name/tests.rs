// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

fn norm(raw: &str) -> String {
    normalise_host(raw)
        .unwrap_or_else(|e| panic!("{raw:?}: {e}"))
        .host()
        .to_string()
}

/// The rule an `Invalid` refusal of `raw` names (the message without the echoed input).
fn reason(raw: &str) -> String {
    let message = normalise_host(raw).unwrap_err().to_string();
    let prefix = format!("invalid host {:?}: ", echo(raw));
    message
        .strip_prefix(&prefix)
        .expect("an Invalid refusal that echoes the input")
        .to_owned()
}

/// Asserts `raw` is refused as a non-canonical number that decodes to `decoded`.
fn assert_decodes_to(raw: &str, decoded: Option<Ipv4Addr>) {
    assert_eq!(
        normalise_host(raw).unwrap_err(),
        NameError::NonCanonicalIp {
            input: raw.to_owned(),
            decoded,
        },
        "{raw}"
    );
}

/// The normalisation table: input, normal form.
#[test]
fn normalisation_table() {
    let table = [
        // Plain names, case, trailing dot.
        ("example.com", "example.com"),
        ("EXAMPLE.com", "example.com"),
        ("example.com.", "example.com"),
        ("a-b.c-d.example", "a-b.c-d.example"),
        ("r3---sn-abc.googlevideo.com", "r3---sn-abc.googlevideo.com"),
        ("1e100.net", "1e100.net"),
        ("dead.cafe", "dead.cafe"),
        ("0x.example.com", "0x.example.com"),
        // IDNA: Unicode to Punycode, mapping of full-width and compatibility forms.
        ("bücher.example", "xn--bcher-kva.example"),
        ("BÜCHER.example", "xn--bcher-kva.example"),
        ("xn--bcher-kva.example", "xn--bcher-kva.example"),
        ("XN--BCHER-KVA.example", "xn--bcher-kva.example"),
        ("ｅｘａｍｐｌｅ.com", "example.com"),
        ("example。com", "example.com"),
        ("faß.de", "xn--fa-hia.de"),
        ("ⓛocalhost", "localhost"),
        // IP literals.
        ("10.0.0.1", "10.0.0.1"),
        ("127.0.0.1.", "127.0.0.1"),
        ("１２７.０.０.１", "127.0.0.1"),
        ("::1", "::1"),
        ("[::1]", "::1"),
        ("[2001:DB8:0:0::1]", "2001:db8::1"),
        ("0:0:0:0:0:0:0:1", "::1"),
        ("::FFFF:127.0.0.1", "::ffff:127.0.0.1"),
    ];
    for (raw, want) in table {
        assert_eq!(norm(raw), want, "{raw:?}");
        // Normal forms are fixed points and pass the shared type's own check.
        assert_eq!(norm(want), want, "{want:?}");
        assert!(Host::parse_normalised(want).is_ok(), "{want:?}");
    }
}

#[test]
fn literals_and_names_land_in_the_right_variant() {
    assert!(matches!(
        normalise_host("[fe80::1]").unwrap().host(),
        Host::Ip(IpAddr::V6(_))
    ));
    assert!(matches!(
        normalise_host("１２７.０.０.１").unwrap().into_host(),
        Host::Ip(IpAddr::V4(ip)) if ip == Ipv4Addr::LOCALHOST
    ));
    assert!(
        normalise_host("example.com")
            .unwrap()
            .host()
            .name()
            .is_some()
    );
    for h in ["localhost", "metadata.goog", "example.com", "10.0.0.1"] {
        let t = normalise_host(h).unwrap();
        assert_eq!(Target::from_host(t.host().clone()), t, "{h}");
    }
}

#[test]
fn refused_hosts_and_why() {
    let table = [
        ("", "is empty"),
        ("user@example.com", "IDNA/LDH"),
        ("a b.com", "IDNA/LDH"),
        ("exa/mple.com", "IDNA/LDH"),
        ("ex\\ample.com", "IDNA/LDH"),
        ("nul\0.com", "IDNA/LDH"),
        ("tab\t.com", "IDNA/LDH"),
        ("under_score.example", "IDNA/LDH"),
        ("percent%41.com", "IDNA/LDH"),
        ("*.example.com", "IDNA/LDH"),
        ("-lead.example", "valid host name"),
        ("trail-.example", "valid host name"),
        ("xn--zz.example", "IDNA/LDH"),
        ("ｕｓｅｒ＠example.com", "IDNA/LDH"),
        (".", "empty label"),
        ("..", "empty label"),
        ("a..b", "empty label"),
        (".example.com", "empty label"),
        ("example.com..", "empty label"),
        ("[::1", "unclosed"),
        ("[10.0.0.1]", "IPv6"),
        ("[]", "IPv6"),
        ("fe80::1%eth0", "zone"),
        ("[fe80::1%25eth0]", "zone"),
        ("::zz", "IPv6"),
        ("host:80", "IPv6"),
    ];
    for (raw, want) in table {
        assert!(reason(raw).contains(want), "{raw:?}: {}", reason(raw));
    }
    let long_label = format!("{}.com", "a".repeat(64));
    assert!(reason(&long_label).contains("63"));
    let long_name = vec!["a".repeat(63); 4].join(".");
    assert!(reason(&long_name).contains("253"));
    assert!(reason(&"a".repeat(MAX_RAW_HOST_LEN + 1)).contains("too long"));
    // The limit itself is still a length the name rules judge, not "too long".
    assert!(!reason(&"a".repeat(MAX_RAW_HOST_LEN)).contains("too long"));
}

#[test]
fn non_canonical_numbers_decode_but_are_refused() {
    let metadata = Some(Ipv4Addr::new(169, 254, 169, 254));
    for raw in [
        "2852039166",
        "0xA9FEA9FE",
        "0251.0376.0251.0376",
        "169.254.43518",
        "0xa9.0xfe.0xa9.0xfe",
    ] {
        assert_decodes_to(raw, metadata);
    }
    for raw in ["0x7f.1", "127.1", "0177.0.0.1", "0x7f000001", "127.0.1"] {
        assert_decodes_to(raw, Some(Ipv4Addr::LOCALHOST));
    }
    assert_decodes_to("010.0.0.1", Some(Ipv4Addr::new(8, 0, 0, 1)));
    assert_decodes_to("0x", Some(Ipv4Addr::UNSPECIFIED));
    // Still refused when it decodes to nothing.
    for raw in [
        "1.2.3.4.5",
        "example.256",
        "999.1.1.1",
        "1.2.3.256",
        "1.2.65536",
        "4294967296",
        "09.1.1.1",
        "0xg.1",
        "a.0x1",
    ] {
        assert_decodes_to(raw, None);
    }
}

#[test]
fn non_canonical_errors_say_what_to_write() {
    let err = normalise_host("127.1").unwrap_err();
    assert_eq!(
        err.to_string(),
        r#""127.1" is not a canonical IP address: it decodes to 127.0.0.1, write that instead"#
    );
    let err = normalise_host("999.1.1.1").unwrap_err();
    assert!(err.to_string().ends_with("decodes to no address"), "{err}");
    let err = normalise_host("a b").unwrap_err();
    assert!(
        err.to_string().starts_with(r#"invalid host "a b": "#),
        "{err}"
    );
}

#[test]
fn echoed_input_is_bounded() {
    let raw = format!("{}!", "x".repeat(200));
    let echoed = echo(&raw);
    assert_eq!(echoed.chars().count(), MAX_ECHOED_CHARS + 1);
    assert!(echoed.ends_with('…'));
    assert!(
        normalise_host(&raw)
            .unwrap_err()
            .to_string()
            .starts_with(&format!("invalid host {echoed:?}: "))
    );
}

#[test]
fn input_of_exactly_the_echo_limit_is_echoed_whole() {
    let raw = format!("{} ", "x".repeat(MAX_ECHOED_CHARS - 1));
    let err = normalise_host(&raw).unwrap_err();
    assert!(matches!(&err, NameError::Invalid { input, .. } if *input == raw));
}

#[test]
fn names_that_are_a_category_by_themselves() {
    for raw in ["localhost", "LOCALHOST.", "foo.localhost", "a.b.localhost"] {
        assert_eq!(
            normalise_host(raw).unwrap().named_category(),
            Some(LocalCategory::Loopback),
            "{raw}"
        );
    }
    for raw in [
        "metadata.google.internal",
        "metadata",
        "instance-data.ec2.internal",
        "instance-data",
        "Metadata.Goog.",
    ] {
        assert_eq!(
            normalise_host(raw).unwrap().named_category(),
            Some(LocalCategory::Metadata),
            "{raw}"
        );
    }
    for raw in [
        "localhost.example.com",
        "notlocalhost",
        "svc.corp.internal",
        "nas.local",
        "127.0.0.1",
    ] {
        assert_eq!(normalise_host(raw).unwrap().named_category(), None, "{raw}");
    }
}

#[test]
fn name_rule_maps_every_domain_name_reason() {
    assert_eq!(name_rule("other"), "is not a valid host name");
}
