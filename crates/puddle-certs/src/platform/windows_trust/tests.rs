// SPDX-License-Identifier: AGPL-3.0-or-later
use std::time::SystemTime;

use super::*;
use crate::devcert::leaf;

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
