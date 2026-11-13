// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use LocalCategory::{LinkLocal, Loopback, Metadata, Private, Special};

fn cat(s: &str) -> Option<LocalCategory> {
    classify_ip(s.parse().unwrap()).category()
}

fn all(inputs: &[&str], want: Option<LocalCategory>) {
    for i in inputs {
        assert_eq!(cat(i), want, "{i}");
    }
}

#[test]
fn a_company_class_audits_under_its_signal_code_and_the_company_category() {
    let class = AddressClass::Company(CompanySignal::DnsSuffix);
    assert_eq!(class.rule(), CompanySignal::DnsSuffix.code());
    assert_eq!(class.category(), Some(LocalCategory::CompanyNetwork));
}

// The reserved-range table, one case per row, with the local-destination categories.

#[test]
fn row01_06_metadata_addresses() {
    all(
        &[
            "169.254.169.254",
            "fd00:ec2::254",
            "fd20:ce::254",
            "168.63.129.16",
            "100.100.100.200",
            "192.0.0.192",
            "169.254.170.2",
            "169.254.170.23",
            "fd00:ec2::23",
        ],
        Some(Metadata),
    );
}

#[test]
fn row07_08_link_local() {
    all(
        &["169.254.1.1", "169.254.255.255", "fe80::1"],
        Some(LinkLocal),
    );
}

#[test]
fn row10_13_loopback_and_unspecified() {
    all(
        &["127.0.0.1", "127.255.255.254", "::1", "0.0.0.0", "::"],
        Some(Loopback),
    );
    all(&["0.1.2.3"], Some(Special));
}

#[test]
fn row14_18_private_cgnat_ula_and_edges() {
    all(
        &["10.0.0.1", "172.16.0.1", "172.31.255.255", "192.168.1.1"],
        Some(Private),
    );
    all(&["172.15.255.255", "172.32.0.0"], None);
    all(&["100.64.0.1", "100.127.255.255"], Some(Private));
    all(&["100.63.255.255", "100.128.0.0"], None);
    all(&["fc00::1", "fdff::1"], Some(Private));
}

#[test]
fn row19_23_special_use() {
    all(
        &[
            "224.0.0.251",
            "ff02::fb",
            "255.255.255.255",
            "240.0.0.1",
            "198.18.0.1",
            "203.0.113.17",
            "2001:db8::1",
            "192.0.0.8",
            "192.88.99.1",
            "3fff::1",
        ],
        Some(Special),
    );
}

#[test]
fn row24_29_embedded_ipv4() {
    assert_eq!(cat("::ffff:169.254.169.254"), Some(Metadata));
    assert_eq!(cat("::ffff:127.0.0.1"), Some(Loopback));
    assert_eq!(cat("::ffff:10.0.0.1"), Some(Private));
    assert_eq!(cat("::ffff:8.8.8.8"), None);
    assert_eq!(cat("::169.254.169.254"), Some(Metadata));
    assert_eq!(cat("::8.8.8.8"), Some(Special));
    assert_eq!(cat("64:ff9b::a9fe:a9fe"), Some(Metadata));
    assert_eq!(cat("64:ff9b::7f00:1"), Some(Loopback));
    assert_eq!(cat("64:ff9b::808:808"), None);
}

#[test]
fn row30_32_reserved_and_transition() {
    all(&["64:ff9b:1::1", "100::1", "fec0::1"], Some(Special));
    assert_eq!(cat("2002:a9fe:a9fe::1"), Some(Metadata));
    assert_eq!(cat("2002:0a00:0001::1"), Some(Private));
    assert_eq!(cat("2002:0808:0808::1"), Some(Special));
    // Teredo 2001:0:4136:e378:..., client 127.0.0.1 inverted = 80ff:fffe.
    assert_eq!(cat("2001:0:4136:e378:8000:63bf:80ff:fffe"), Some(Loopback));
    assert_eq!(cat("2001:0:4136:e378::1"), Some(Special));
    assert_eq!(cat("2001:2::1"), Some(Special));
}

#[test]
fn row39_public() {
    all(
        &[
            "8.8.8.8",
            "140.82.112.3",
            "2606:4700::1111",
            "2a00:1450:4001:80b::200e",
        ],
        None,
    );
}

#[test]
fn rules_name_the_reason() {
    let rule = |s: &str| classify_ip(s.parse().unwrap()).rule();
    assert_eq!(rule("10.0.0.1"), "rfc1918");
    assert_eq!(rule("100.64.0.1"), "cgnat");
    assert_eq!(rule("::ffff:10.0.0.1"), "ipv4-mapped");
    assert_eq!(rule("64:ff9b::a9fe:a9fe"), "nat64");
    assert_eq!(rule("2002:0808:0808::1"), "6to4");
    assert_eq!(rule("8.8.8.8"), "public");
    assert_eq!(
        AddressClass::PuddleEndpoint(EndpointKind::Api).rule(),
        "puddle_endpoint"
    );
    assert_eq!(
        AddressClass::PuddleEndpoint(EndpointKind::Api).category(),
        None
    );
}
