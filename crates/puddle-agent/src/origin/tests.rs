// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn a_source_none_of_the_guests_interfaces_has_is_a_containers() {
    let own = [Ipv4Addr::new(10, 0, 2, 15), Ipv4Addr::new(172, 17, 0, 1)];
    let peer = |s: &str| s.parse::<SocketAddr>().unwrap();
    // A container on the docker0 bridge.
    assert!(is_foreign(peer("172.17.0.2:41000"), &own));
    // The guest's own processes: the outbound interface, the bridge's own address, loopback.
    assert!(!is_foreign(peer("10.0.2.15:41000"), &own));
    assert!(!is_foreign(peer("172.17.0.1:41000"), &own));
    assert!(!is_foreign(peer("127.0.0.1:41000"), &own));
    assert!(!is_foreign(peer("[::1]:41000"), &own));
}

mod listing {
    use std::io;

    use super::*;
    use crate::bridge::{IfAddr, Interfaces};

    struct Fake(Vec<IfAddr>);

    impl Interfaces for Fake {
        fn ipv4(&self) -> io::Result<Vec<IfAddr>> {
            Ok(self.0.clone())
        }
    }

    struct Broken;

    impl Interfaces for Broken {
        fn ipv4(&self) -> io::Result<Vec<IfAddr>> {
            Err(io::Error::other("netlink says no"))
        }
    }

    fn peer(s: &str) -> SocketAddr {
        s.parse().unwrap()
    }

    #[test]
    fn the_listing_decides_which_sources_are_the_guests_own() {
        let own = Fake(vec![IfAddr {
            name: "eth0".into(),
            index: 2,
            addr: Ipv4Addr::new(10, 0, 2, 15),
        }]);
        assert!(!from_container_by(peer("10.0.2.15:40000"), &own));
        assert!(from_container_by(peer("172.17.0.2:40000"), &own));
    }

    #[test]
    fn a_listing_that_fails_marks_every_ipv4_client_but_loopback() {
        assert!(from_container_by(peer("172.17.0.2:40000"), &Broken));
        assert!(!from_container_by(peer("127.0.0.1:40000"), &Broken));
        assert!(!from_container_by(peer("[2001:db8::1]:40000"), &Broken));
    }

    #[test]
    fn the_real_listing_does_not_count_loopback_as_a_container() {
        assert!(!from_container(peer("127.0.0.1:40000")));
    }
}
