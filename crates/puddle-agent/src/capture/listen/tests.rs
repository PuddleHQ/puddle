// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn a_connection_dialled_at_the_port_was_not_redirected() {
    let at = |s: &str| Some(s.parse::<std::net::SocketAddr>().unwrap());
    let orig = |s: &str| s.parse::<SocketAddrV4>().unwrap();
    // Arrived on the address it was "originally" going to: nobody redirected it.
    assert!(!was_redirected(
        orig("172.17.0.1:3129"),
        at("172.17.0.1:3129")
    ));
    assert!(!was_redirected(
        orig("127.0.0.1:3129"),
        at("127.0.0.1:3129")
    ));
    // Loopback is never a redirect target, even if the addresses differ.
    assert!(!was_redirected(orig("127.0.0.2:80"), at("127.0.0.1:3129")));
    // Redirected: the original destination is somewhere else.
    assert!(was_redirected(
        orig("93.184.216.34:443"),
        at("127.0.0.1:3129")
    ));
    assert!(was_redirected(
        orig("198.18.0.9:443"),
        at("172.17.0.1:3129")
    ));
    assert!(was_redirected(orig("93.184.216.34:443"), None));
}

#[test]
fn a_real_address_is_asked_for_by_address() {
    let table = StandIns::in_memory();
    let (dest, hold) = destination("93.184.216.34:443".parse().unwrap(), &table);
    assert_eq!(
        dest,
        Destination::Address(Ipv4Addr::new(93, 184, 216, 34), 443)
    );
    assert_eq!(dest.authority().as_deref(), Some("93.184.216.34:443"));
    assert!(hold.is_none());
}

#[test]
fn a_stand_in_is_asked_for_by_name_and_held_while_the_connection_lives() {
    let table = StandIns::in_memory();
    let ip = table.address_for("db.example.com").unwrap();
    let (dest, hold) = destination(SocketAddrV4::new(ip, 5432), &table);
    assert_eq!(dest.authority().as_deref(), Some("db.example.com:5432"));
    assert_eq!(hold.unwrap().name(), "db.example.com");
}

#[test]
fn a_stand_in_nobody_handed_out_is_unknown_not_a_guess() {
    let table = StandIns::in_memory();
    let (dest, hold) = destination("198.18.5.5:443".parse().unwrap(), &table);
    assert_eq!(dest, Destination::Unknown);
    assert_eq!(dest.authority(), None);
    assert!(hold.is_none());
}
