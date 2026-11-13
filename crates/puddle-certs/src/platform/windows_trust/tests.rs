// SPDX-License-Identifier: AGPL-3.0-or-later
use std::time::SystemTime;

use super::*;

fn leaf_der() -> Vec<u8> {
    leaf::generate(SystemTime::now()).unwrap().cert_der
}

#[test]
fn adding_to_a_store_trusts_once_and_keeps_one_copy() {
    let store = Store::memory().unwrap();
    let der = leaf_der();
    assert!(!store.contains(&der));
    assert_eq!(store.add(&der).unwrap(), TrustOutcome::Trusted);
    assert_eq!(store.add(&der).unwrap(), TrustOutcome::Trusted);
    assert!(store.contains(&der));
    let mut count = 0;
    store.each(|_| {
        count += 1;
        true
    });
    assert_eq!(count, 1, "an equal certificate is kept, not duplicated");
    assert!(store.add(b"not a certificate").is_err());
}

#[test]
fn only_certificates_with_the_marker_are_listed_and_trust_is_looked_up() {
    let (my, roots) = (Store::memory().unwrap(), Store::memory().unwrap());
    let dev = leaf_der();
    let plain = {
        let mut p = rcgen::CertificateParams::new(vec!["example.test".to_owned()]).unwrap();
        p.is_ca = rcgen::IsCa::NoCa;
        let key = rcgen::KeyPair::generate().unwrap();
        p.self_signed(&key).unwrap().der().to_vec()
    };
    my.add(&dev).unwrap();
    my.add(&plain).unwrap();
    let listed = existing_in(&my, Some(&roots));
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].der, dev);
    assert!(!listed[0].trusted);
    assert_eq!(
        listed[0].key,
        KeyAccess::Missing,
        "a certificate with no key"
    );

    roots.add(&dev).unwrap();
    assert!(existing_in(&my, Some(&roots))[0].trusted);
    assert!(!existing_in(&my, None)[0].trusted);
}
