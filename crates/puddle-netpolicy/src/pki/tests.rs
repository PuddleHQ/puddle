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

#[test]
fn a_path_that_climbs_out_of_the_url_is_never_covered() {
    let p = points(&[
        "http://pki.corp.example/crl/root.crl",
        "http://pki.corp.example/crl/",
        "http://ocsp.corp.example/ocsp",
    ]);
    let pki = host("pki.corp.example");
    let ocsp = host("ocsp.corp.example");
    for path in [
        "/crl/../intranet/payroll.xlsx",
        "/crl/root.crl/../../admin/secret",
        "/crl/./../admin",
        "/crl/%2e%2e/intranet",
        "/crl/%2E%2E/intranet",
        "/crl/.%2e/intranet",
        "/crl/%2e./intranet",
        "/crl/..%2fintranet",
        "/crl/..%2Fintranet",
        "/crl/%2e%2e%2fintranet",
        "/crl/..\\intranet",
        "/crl/..%5cintranet",
        "/crl/%5c..%5cintranet",
        "/crl/..;/intranet",
        "/crl/%252e%252e/intranet",
        "/crl//../intranet",
        "/crl/...",
        "/crl/.. /admin",
        "/crl/..%20/admin",
        "/crl/.",
        "/crl/%2e",
        "/crl/root.crl%00/../x",
        "/crl/%zz",
        "/crl/%2",
    ] {
        assert!(!p.covers(&pki, 80, "GET", path), "{path}");
    }
    assert!(!p.covers(&ocsp, 80, "GET", "/ocsp/.."));
    assert!(!p.covers(&ocsp, 80, "GET", "/ocsp/%2e%2e%2fadmin"));
}

#[test]
fn an_ocsp_get_with_an_escaped_base64_request_is_still_covered() {
    // RFC 6960 A.1: the request is the URL-escaped base64 of the DER request, so `/`, `+` and `=`
    // arrive escaped; an escaped `/` must not stop a real OCSP lookup.
    let p = points(&["http://ocsp.corp.example/ocsp"]);
    let h = host("ocsp.corp.example");
    assert!(p.covers(&h, 80, "GET", "/ocsp/MFEwTzBN%2FMEsw%2BSTAJ%3D%3D"));
    assert!(p.covers(&h, 80, "GET", "/ocsp/MFEwTzBN/MEsw+STAJ=="));
    let crl = points(&["http://pki.corp.example/crl/"]);
    assert!(crl.covers(
        &host("pki.corp.example"),
        80,
        "GET",
        "/crl/Corp%20Root%20CA.crl"
    ));
    assert!(crl.covers(&host("pki.corp.example"), 80, "GET", "/crl/root.v2.crl"));
}
