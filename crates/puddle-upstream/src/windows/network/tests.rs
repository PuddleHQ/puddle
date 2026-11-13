// SPDX-License-Identifier: GPL-3.0-or-later
//! The Windows reader against the machine it runs on: nothing is mocked, so the assertions are
//! the facts every Windows machine has.

use std::net::{IpAddr, Ipv4Addr};

use super::*;

#[test]
fn the_machine_has_a_loopback_adapter_and_a_loopback_route() {
    let snapshot = WinNetwork.read().expect("the network can be read");
    assert!(
        snapshot
            .adapters
            .iter()
            .any(|adapter| adapter.kind == AdapterKind::Loopback
                && adapter
                    .subnets
                    .iter()
                    .any(|(ip, _)| *ip == IpAddr::V4(Ipv4Addr::LOCALHOST))),
        "no loopback adapter in {:?}",
        snapshot.adapters
    );
    assert!(
        snapshot
            .routes
            .iter()
            .any(|route| route.net == IpAddr::V4(Ipv4Addr::new(127, 0, 0, 0)) && route.bits == 8),
        "no 127.0.0.0/8 route in {:?}",
        snapshot.routes
    );
}

#[test]
fn every_route_leaves_by_an_adapter_the_read_lists() {
    let snapshot = WinNetwork.read().expect("the network can be read");
    for route in &snapshot.routes {
        assert!(
            snapshot
                .adapters
                .iter()
                .any(|adapter| adapter.index == route.interface),
            "route {route:?} leaves by an adapter that is not listed"
        );
    }
}

#[test]
fn a_registry_value_that_is_not_there_reads_as_nothing() {
    let missing = r"SOFTWARE\Puddle\NoSuchKey";
    assert_eq!(
        registry_strings(HKEY_LOCAL_MACHINE, missing, "Name"),
        Vec::<String>::new()
    );
    assert_eq!(subkeys(missing), Vec::<String>::new());
}

#[test]
fn the_tcpip_key_lists_its_value_as_strings_when_it_has_one() {
    // `Hostname` is a plain string on every Windows machine: a read of it is one entry.
    let host = registry_strings(HKEY_LOCAL_MACHINE, TCPIP_PARAMETERS, "Hostname");
    assert_eq!(host.len(), 1, "{host:?}");
    assert_ne!(host[0], "");
}
