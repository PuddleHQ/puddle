// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn the_key_reads_back_as_der_and_pem() {
    let key = PrivateKey::from_pkcs8_der(vec![1, 2, 3, 4]);
    assert_eq!(key.pkcs8_der(), [1, 2, 3, 4]);
    let pem = key.to_pem();
    assert_eq!(pem::parse(pem.as_bytes()).unwrap().contents(), [1, 2, 3, 4]);
    assert!(pem.starts_with("-----BEGIN PRIVATE KEY-----"));
    assert_eq!(format!("{key:?}"), "PrivateKey(..)");
}
