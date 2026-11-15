// SPDX-License-Identifier: AGPL-3.0-or-later
use std::time::Duration;

use super::*;
use crate::store::SOURCES;

const LM_ROOT: StoreSource = SOURCES[0];
const LM_ROOT_GP: StoreSource = SOURCES[1];
const CU_ROOT: StoreSource = SOURCES[3];
const LM_CA: StoreSource = SOURCES[5];
const CU_CA: StoreSource = SOURCES[8];
const LM_DISALLOWED: StoreSource = SOURCES[10];
const CU_DISALLOWED: StoreSource = SOURCES[11];

/// 2030-01-01, a fixed "now".
fn now() -> SystemTime {
    UNIX_EPOCH + Duration::from_hours(525_960)
}

struct Ca {
    key: rcgen::KeyPair,
    cert: rcgen::Certificate,
    params: rcgen::CertificateParams,
}

fn params(cn: &str, not_after_year: i32) -> rcgen::CertificateParams {
    let mut p = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
    p.distinguished_name.push(rcgen::DnType::CommonName, cn);
    p.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    p.not_after = rcgen::date_time_ymd(not_after_year, 1, 1);
    p
}

fn root(cn: &str, year: i32) -> Ca {
    let key = rcgen::KeyPair::generate().unwrap();
    let p = params(cn, year);
    let cert = p.self_signed(&key).unwrap();
    Ca {
        key,
        cert,
        params: p,
    }
}

fn issued(cn: &str, year: i32, by: &Ca) -> Ca {
    let key = rcgen::KeyPair::generate().unwrap();
    let p = params(cn, year);
    let issuer = rcgen::Issuer::from_params(&by.params, &by.key);
    let cert = p.signed_by(&key, &issuer).unwrap();
    Ca {
        key,
        cert,
        params: p,
    }
}

fn der(ca: &Ca) -> Vec<u8> {
    ca.cert.der().to_vec()
}

fn names(roots: &CorporateRoots) -> Vec<&str> {
    let mut n: Vec<&str> = roots
        .certificates()
        .iter()
        .map(|c| c.subject_cn().unwrap())
        .collect();
    n.sort_unstable();
    n
}

fn skipped(roots: &CorporateRoots) -> Vec<(String, SkipReason)> {
    let mut s: Vec<(String, SkipReason)> = roots
        .skipped()
        .iter()
        .map(|c| (c.subject_cn.clone().unwrap_or_default(), c.reason))
        .collect();
    s.sort();
    s
}

#[test]
fn admin_and_user_roots_are_synced_disallowed_and_expired_are_not() {
    let corp = root("Corp Root", 2040);
    let user = root("Fiddler Root", 2040);
    let banned = root("Banned Root", 2040);
    let old = root("Old Root", 2029);
    let mut s = StoreSnapshot::new();
    s.add(LM_ROOT_GP, der(&corp));
    s.add(CU_ROOT, der(&user));
    s.add(LM_ROOT, der(&banned));
    s.add(CU_DISALLOWED, der(&banned));
    s.add(LM_ROOT, der(&old));
    let r = CorporateRoots::select(&s, now());
    assert_eq!(names(&r), ["Corp Root", "Fiddler Root"]);
    assert!(r.certificates().iter().all(|c| c.kind() == CertKind::Root));
    assert_eq!(
        skipped(&r),
        [
            ("Banned Root".to_owned(), SkipReason::Disallowed),
            ("Old Root".to_owned(), SkipReason::Expired),
        ]
    );
    let corp_synced = r
        .certificates()
        .iter()
        .find(|c| c.subject_cn() == Some("Corp Root"))
        .unwrap();
    assert_eq!(corp_synced.der(), der(&corp));
    assert_eq!(corp_synced.sources(), [LM_ROOT_GP]);
    assert_eq!(corp_synced.not_after_unix(), 2_208_988_800);
    assert!(!r.is_empty());
}

#[test]
fn a_certificate_that_ends_this_second_is_still_synced() {
    let edge = root("Edge Root", 2030);
    let mut s = StoreSnapshot::new();
    s.add(LM_ROOT, der(&edge));
    let r = CorporateRoots::select(&s, now());
    assert_eq!(names(&r), ["Edge Root"]);
    let later = CorporateRoots::select(&s, now() + Duration::from_secs(1));
    assert_eq!(names(&later), Vec::<&str>::new());
    assert_eq!(
        skipped(&later),
        [("Edge Root".to_owned(), SkipReason::Expired)]
    );
}

#[test]
fn a_disallowed_key_counts_even_in_a_reissued_certificate() {
    let original = root("Rotated Root", 2040);
    // Same key, new certificate (e.g. re-issued with a longer validity).
    let mut p = params("Rotated Root", 2045);
    p.serial_number = Some(rcgen::SerialNumber::from(vec![7]));
    let reissued = p.self_signed(&original.key).unwrap();
    let mut s = StoreSnapshot::new();
    s.add(LM_DISALLOWED, der(&original));
    s.add(LM_ROOT, reissued.der().to_vec());
    let r = CorporateRoots::select(&s, now());
    assert!(r.is_empty());
    assert_eq!(
        skipped(&r),
        [("Rotated Root".to_owned(), SkipReason::Disallowed)]
    );
}

#[test]
fn a_certificate_in_several_stores_is_synced_once_with_every_source() {
    let corp = root("Corp Root", 2040);
    let mut s = StoreSnapshot::new();
    for source in [CU_ROOT, LM_ROOT, LM_ROOT_GP, LM_ROOT, LM_CA] {
        s.add(source, der(&corp));
    }
    let r = CorporateRoots::select(&s, now());
    assert_eq!(r.certificates().len(), 1);
    let c = &r.certificates()[0];
    assert_eq!(c.kind(), CertKind::Root, "in a Root store, so a root");
    assert_eq!(c.sources(), [LM_ROOT, LM_ROOT_GP, LM_CA, CU_ROOT]);
}

#[test]
fn intermediates_need_a_synced_issuer_and_chain_in_any_order() {
    let corp = root("Corp Root", 2040);
    let issuing = issued("Corp Issuing CA", 2039, &corp);
    let proxy = issued("Corp Proxy CA", 2038, &issuing);
    let public_root = root("Public Root (AuthRoot, not read)", 2040);
    let public_int = issued("Public Intermediate", 2039, &public_root);
    let expired_int = issued("Corp Old Issuing CA", 2029, &corp);
    let mut s = StoreSnapshot::new();
    // Grandchild first, so one pass can't place it.
    s.add(CU_CA, der(&proxy));
    s.add(LM_CA, der(&issuing));
    s.add(CU_CA, der(&public_int));
    s.add(LM_CA, der(&expired_int));
    s.add(LM_ROOT, der(&corp));
    let r = CorporateRoots::select(&s, now());
    assert_eq!(names(&r), ["Corp Issuing CA", "Corp Proxy CA", "Corp Root"]);
    let kind = |cn| {
        r.certificates()
            .iter()
            .find(|c| c.subject_cn() == Some(cn))
            .unwrap()
            .kind()
    };
    assert_eq!(kind("Corp Proxy CA"), CertKind::Intermediate);
    assert_eq!(kind("Corp Root"), CertKind::Root);
    assert_eq!(
        skipped(&r),
        [
            ("Corp Old Issuing CA".to_owned(), SkipReason::Expired),
            (
                "Public Intermediate".to_owned(),
                SkipReason::NotIssuedBySyncedCert
            ),
        ]
    );
}

#[test]
fn an_intermediate_under_a_disallowed_root_is_not_synced() {
    let corp = root("Corp Root", 2040);
    let issuing = issued("Corp Issuing CA", 2039, &corp);
    let mut s = StoreSnapshot::new();
    s.add(LM_ROOT, der(&corp));
    s.add(LM_DISALLOWED, der(&corp));
    s.add(LM_CA, der(&issuing));
    let r = CorporateRoots::select(&s, now());
    assert!(r.is_empty());
    assert_eq!(
        skipped(&r),
        [
            (
                "Corp Issuing CA".to_owned(),
                SkipReason::NotIssuedBySyncedCert
            ),
            ("Corp Root".to_owned(), SkipReason::Disallowed),
        ]
    );
}

#[test]
fn unreadable_certificates_and_stores_are_reported_not_synced() {
    let mut s = StoreSnapshot::new();
    s.add(LM_ROOT, b"not a certificate".to_vec());
    // Garbage in Disallowed still blocks the identical bytes, and breaks nothing.
    s.add(CU_DISALLOWED, b"junk".to_vec());
    s.note_unreadable(CU_ROOT, "os error 0x5");
    let r = CorporateRoots::select(&s, now());
    assert!(r.is_empty());
    assert_eq!(r.skipped().len(), 1);
    assert_eq!(r.skipped()[0].reason, SkipReason::Unreadable);
    assert_eq!(r.skipped()[0].subject_cn, None);
    assert_eq!(r.unreadable_stores()[0].source, CU_ROOT);
}

#[test]
fn the_same_stores_in_any_order_give_the_same_selection() {
    let a = root("A", 2040);
    let b = root("B", 2040);
    let c = root("C", 2040);
    let mut one = StoreSnapshot::new();
    let mut two = StoreSnapshot::new();
    for x in [&a, &b, &c] {
        one.add(LM_ROOT, der(x));
    }
    for x in [&c, &a, &b] {
        two.add(LM_ROOT, der(x));
    }
    let (r1, r2) = (
        CorporateRoots::select(&one, now()),
        CorporateRoots::select(&two, now()),
    );
    assert_eq!(r1, r2);
    let fps: Vec<Fingerprint> = r1
        .certificates()
        .iter()
        .map(SyncedCert::fingerprint)
        .collect();
    let mut sorted = fps.clone();
    sorted.sort();
    assert_eq!(fps, sorted);
}

#[test]
fn fingerprints_print_as_hex_and_pem_round_trips() {
    let a = root("A", 2040);
    let mut s = StoreSnapshot::new();
    s.add(LM_ROOT, der(&a));
    let r = CorporateRoots::select(&s, now());
    let c = &r.certificates()[0];
    let hex = c.fingerprint().to_string();
    assert_eq!(hex.len(), 64);
    assert!(
        hex.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    );
    assert_eq!(
        c.fingerprint().as_bytes(),
        &<[u8; 32]>::from(Sha256::digest(der(&a)))
    );
    let pem = c.pem();
    assert!(pem.starts_with("-----BEGIN CERTIFICATE-----\n"));
    assert!(pem.ends_with("-----END CERTIFICATE-----\n"));
    assert!(!pem.contains('\r'));
    assert_eq!(pem::parse(&pem).unwrap().contents(), der(&a));
}

#[test]
fn skip_reasons_read_well_and_times_before_1970_work() {
    assert_eq!(
        SkipReason::Disallowed.to_string(),
        "distrusted by Windows (Disallowed store)"
    );
    assert_eq!(SkipReason::Expired.to_string(), "expired");
    assert_eq!(
        SkipReason::Unreadable.to_string(),
        "not a readable certificate"
    );
    assert_eq!(
        SkipReason::NotIssuedBySyncedCert.to_string(),
        "intermediate not issued by a synced root"
    );
    assert_eq!(unix_seconds(UNIX_EPOCH - Duration::from_secs(5)), -5);
    assert_eq!(unix_seconds(UNIX_EPOCH + Duration::from_secs(5)), 5);
}

fn root_with_crl(cn: &str, uri: &str) -> Ca {
    let key = rcgen::KeyPair::generate().unwrap();
    let mut p = params(cn, 2040);
    p.crl_distribution_points = vec![rcgen::CrlDistributionPoint {
        uris: vec![uri.to_owned()],
    }];
    let cert = p.self_signed(&key).unwrap();
    Ca {
        key,
        cert,
        params: p,
    }
}

#[test]
fn the_urls_a_synced_certificate_names_are_listed_and_a_skipped_ones_are_not() {
    let corp = root_with_crl("Corp Root", "http://pki.corp.test/root.crl");
    let banned = root_with_crl("Banned Root", "http://evil.test/x.crl");
    let old = {
        let key = rcgen::KeyPair::generate().unwrap();
        let mut p = params("Old Root", 2029);
        p.crl_distribution_points = vec![rcgen::CrlDistributionPoint {
            uris: vec!["http://old.test/x.crl".to_owned()],
        }];
        let cert = p.self_signed(&key).unwrap();
        Ca {
            key,
            cert,
            params: p,
        }
    };
    let mut s = StoreSnapshot::new();
    s.add(LM_ROOT_GP, der(&corp));
    s.add(LM_ROOT, der(&banned));
    s.add(CU_DISALLOWED, der(&banned));
    s.add(LM_ROOT, der(&old));
    let r = CorporateRoots::select(&s, now());
    assert_eq!(r.distribution_urls(), ["http://pki.corp.test/root.crl"]);
    assert_eq!(
        CorporateRoots::default().distribution_urls(),
        Vec::<String>::new()
    );
}
