// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use crate::SOURCES;

#[test]
fn open_args_pick_the_physical_or_location_store() {
    let name = |v: &[u16]| String::from_utf16(v.strip_suffix(&[0]).unwrap()).unwrap();
    let (p, f, n) = open_args(SOURCES[0]);
    assert_eq!(
        (p, name(&n)),
        (CERT_STORE_PROV_PHYSICAL_W, "Root\\.Default".to_owned())
    );
    assert_eq!(f & 0xffff_0000, CERT_SYSTEM_STORE_LOCAL_MACHINE);
    let (p, f, n) = open_args(SOURCES[4]);
    assert_eq!((p, name(&n)), (CERT_STORE_PROV_SYSTEM_W, "Root".to_owned()));
    assert_eq!(f & 0xffff_0000, CERT_SYSTEM_STORE_CURRENT_USER_GROUP_POLICY);
    let (_, f, n) = open_args(SOURCES[7]);
    assert_eq!(name(&n), "CA");
    assert_eq!(f & 0xffff_0000, CERT_SYSTEM_STORE_LOCAL_MACHINE_ENTERPRISE);
    let (p, f, n) = open_args(SOURCES[11]);
    assert_eq!(
        (p, name(&n)),
        (CERT_STORE_PROV_SYSTEM_W, "Disallowed".to_owned())
    );
    assert_eq!(f & 0xffff_0000, CERT_SYSTEM_STORE_CURRENT_USER);
    assert_ne!(f & CERT_STORE_READONLY_FLAG, 0);
}

#[test]
fn the_end_of_a_store_is_not_an_error_but_any_other_stop_is() {
    let from = |items: Vec<Vec<u8>>| {
        let mut items = items.into_iter();
        move || items.next()
    };
    let not_found = CRYPT_E_NOT_FOUND.cast_unsigned();
    let (read, failure) = drain(from(vec![vec![1], vec![2]]), || not_found);
    assert_eq!((read.len(), failure), (2, None));
    let (read, failure) = drain(from(vec![vec![1]]), || ERROR_SUCCESS);
    assert_eq!(
        (read.len(), failure),
        (1, None),
        "no error recorded is the end"
    );
    let (read, failure) = drain(from(vec![vec![1], vec![2], vec![3]]), || 0x5);
    assert_eq!(
        (read.len(), failure),
        (3, Some(0x5)),
        "access denied is a failure"
    );
}

#[test]
fn every_source_reads_without_error() {
    let snapshot = read(&SOURCES).unwrap();
    // A Windows host always has some machine roots (.Default holds the OS's own).
    assert!(
        snapshot.certs().iter().any(|c| c.source == SOURCES[0]),
        "no LocalMachine\\Root\\.Default certificates"
    );
    assert!(
        snapshot.unreadable().is_empty(),
        "{:?}",
        snapshot.unreadable()
    );
}
