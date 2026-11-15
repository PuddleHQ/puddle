// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

/// The DER of an AIA extension value with one access description per `(method last byte, uri)`
/// (`1.3.6.1.5.5.7.48.<n>`: 1 is OCSP, 2 is caIssuers).
fn aia(descriptions: &[(u8, &str)]) -> Vec<u8> {
    let mut body = Vec::new();
    for (method, uri) in descriptions {
        let oid = [
            0x06, 0x08, 0x2b, 0x06, 0x01, 0x05, 0x05, 0x07, 0x30, *method,
        ];
        let location = [
            &[0x86, u8::try_from(uri.len()).unwrap()][..],
            uri.as_bytes(),
        ]
        .concat();
        let inner = [&oid[..], &location[..]].concat();
        body.extend([&[0x30, u8::try_from(inner.len()).unwrap()][..], &inner].concat());
    }
    [&[0x30, u8::try_from(body.len()).unwrap()][..], &body].concat()
}

fn cert(cdp: &[&str], access: &[(u8, &str)]) -> Vec<u8> {
    let mut p = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
    p.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    if !cdp.is_empty() {
        p.crl_distribution_points = vec![rcgen::CrlDistributionPoint {
            uris: cdp.iter().map(|u| (*u).to_owned()).collect(),
        }];
    }
    if !access.is_empty() {
        p.custom_extensions
            .push(rcgen::CustomExtension::from_oid_content(
                &[1, 3, 6, 1, 5, 5, 7, 1, 1],
                aia(access),
            ));
    }
    let key = rcgen::KeyPair::generate().unwrap();
    p.self_signed(&key).unwrap().der().to_vec()
}

#[test]
fn crl_points_and_aia_locations_are_read_sorted_once() {
    let der = cert(
        &[
            "http://pki.corp.test/crl/root.crl",
            "http://pki.corp.test/crl/root.crl",
        ],
        &[
            (2, "http://pki.corp.test/ca.crt"),
            (1, "http://ocsp.corp.test/"),
        ],
    );
    assert_eq!(
        http_urls(&der),
        [
            "http://ocsp.corp.test/",
            "http://pki.corp.test/ca.crt",
            "http://pki.corp.test/crl/root.crl",
        ]
    );
}

#[test]
fn only_plain_http_urls_count_and_the_scheme_is_not_case_sensitive() {
    let der = cert(
        &[
            "ldap://dir.corp.test/cn=crl",
            "https://pki.corp.test/a.crl",
            "HTTP://pki.corp.test/b.crl",
        ],
        &[(2, "http://pki.corp.test/c.crt")],
    );
    assert_eq!(
        http_urls(&der),
        ["HTTP://pki.corp.test/b.crl", "http://pki.corp.test/c.crt"]
    );
}

#[test]
fn an_access_method_that_is_neither_issuer_nor_ocsp_is_not_read() {
    // 1.3.6.1.5.5.7.48.5 is caRepository.
    let der = cert(&[], &[(5, "http://repo.corp.test/")]);
    assert_eq!(http_urls(&der), Vec::<String>::new());
}

#[test]
fn a_certificate_without_the_extensions_or_unreadable_gives_none() {
    assert_eq!(http_urls(&cert(&[], &[])), Vec::<String>::new());
    assert_eq!(http_urls(b"not a certificate"), Vec::<String>::new());
}
