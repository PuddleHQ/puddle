// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

fn host(name: &str) -> Host {
    Host::parse_normalised(name).unwrap()
}

fn points(urls: &[&str]) -> PkiDistributionPoints {
    PkiDistributionPoints::from_urls(urls)
}

#[test]
fn a_url_covers_its_exact_scheme_host_port_and_path() {
    let p = points(&["http://pki.corp.example/crl/root.crl"]);
    assert!(p.covers(&host("pki.corp.example"), 80, "GET", "/crl/root.crl"));
    assert!(p.covers(&host("pki.corp.example"), 80, "HEAD", "/crl/root.crl"));
    assert!(p.covers(&host("pki.corp.example"), 80, "get", "/crl/root.crl"));
    assert!(!p.covers(&host("pki.corp.example"), 80, "GET", "/crl/other.crl"));
    assert!(!p.covers(&host("pki.corp.example"), 80, "GET", "/crl"));
    assert!(!p.covers(&host("pki.corp.example"), 8080, "GET", "/crl/root.crl"));
    assert!(!p.covers(&host("other.corp.example"), 80, "GET", "/crl/root.crl"));
    assert!(!p.covers(&host("sub.pki.corp.example"), 80, "GET", "/crl/root.crl"));
}

#[test]
fn only_get_and_head_are_covered() {
    let p = points(&["http://ocsp.corp.example/"]);
    for method in ["POST", "PUT", "DELETE", "CONNECT", "OPTIONS", ""] {
        assert!(
            !p.covers(&host("ocsp.corp.example"), 80, method, "/"),
            "{method}"
        );
    }
}

#[test]
fn a_path_below_the_url_is_covered_only_as_a_directory() {
    let p = points(&["http://ocsp.corp.example/ocsp"]);
    let h = host("ocsp.corp.example");
    assert!(p.covers(&h, 80, "GET", "/ocsp"));
    assert!(p.covers(&h, 80, "GET", "/ocsp/MFEwTzBNMEsw"));
    assert!(!p.covers(&h, 80, "GET", "/ocsp.evil"));
    assert!(!p.covers(&h, 80, "GET", "/ocspx/a"));
    let root = points(&["http://ocsp.corp.example"]);
    assert!(root.covers(&h, 80, "GET", "/MFEwTzBNMEsw"));
    assert!(root.covers(&h, 80, "GET", "/"));
}

#[test]
fn the_port_and_an_ip_literal_are_read() {
    let p = points(&["http://10.1.2.3:8080/a.crl", "http://[fd00::5]/b.crl"]);
    assert!(p.covers(&host("10.1.2.3"), 8080, "GET", "/a.crl"));
    assert!(p.covers(&host("fd00::5"), 80, "GET", "/b.crl"));
    assert!(!p.covers(&host("10.1.2.3"), 80, "GET", "/a.crl"));
}

#[test]
fn a_name_is_normalised_and_the_query_is_not_part_of_the_url() {
    let p = points(&["HTTP://PKI.Corp.Example./a.crl?x=1#f"]);
    assert!(p.covers(&host("pki.corp.example"), 80, "GET", "/a.crl"));
}

#[test]
fn a_url_that_names_nothing_usable_is_left_out() {
    let p = points(&[
        "https://pki.corp.example/a.crl",
        "ldap://dir.corp.example/cn=x",
        "http://user@pki.corp.example/a.crl",
        "http://pki.corp.example:0/a.crl",
        "http://pki.corp.example:99999/a.crl",
        "http://pki.corp.example:x/a.crl",
        "http://[fd00::5/a.crl",
        "http://[fd00::5]x/a.crl",
        "http://127.1/a.crl",
        "http:///a.crl",
        "http",
        "",
    ]);
    assert!(p.is_empty());
}

#[test]
fn repeats_count_once() {
    let p = points(&[
        "http://a.corp.example/x",
        "http://a.corp.example/x",
        "http://b.corp.example/x",
    ]);
    assert_eq!(p.len(), 2);
    assert!(!p.is_empty());
    assert!(PkiDistributionPoints::default().is_empty());
}
