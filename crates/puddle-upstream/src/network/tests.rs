// SPDX-License-Identifier: AGPL-3.0-or-later
//! Tests of the network reader seam.

use super::*;

#[cfg(not(windows))]
#[test]
fn the_system_network_is_unread_where_there_is_no_reader() {
    assert!(system_network().read().is_err());
}

#[test]
fn interface_types_map_to_their_kinds() {
    let cases = [
        (6, AdapterKind::Ethernet),
        (71, AdapterKind::Wireless),
        (243, AdapterKind::Cellular),
        (244, AdapterKind::Cellular),
        (23, AdapterKind::PointToPoint),
        (108, AdapterKind::PointToPoint),
        (131, AdapterKind::Tunnel),
        (24, AdapterKind::Loopback),
        (53, AdapterKind::Other),
    ];
    for (if_type, kind) in cases {
        assert_eq!(AdapterKind::from_if_type(if_type), kind, "ifType {if_type}");
    }
}

#[test]
fn a_platform_without_a_reader_says_so() {
    let err = UnreadNetwork.read().expect_err("no reader");
    assert!(err.to_string().contains("no network reader"), "{err}");
}

#[test]
fn the_fake_reports_what_it_was_given_and_counts_reads() {
    let fake = FakeNetwork::new(NetworkSnapshot::default());
    assert_eq!(fake.read().expect("read"), NetworkSnapshot::default());
    let changed = NetworkSnapshot {
        dns_namespaces: vec!["corp.example".to_owned()],
        ..NetworkSnapshot::default()
    };
    fake.set(changed.clone());
    assert_eq!(fake.read().expect("read"), changed);
    assert_eq!(fake.reads(), 2);
    assert!(FakeNetwork::failing("down").read().is_err());
}
