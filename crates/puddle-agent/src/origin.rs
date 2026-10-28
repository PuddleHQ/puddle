// SPDX-License-Identifier: GPL-3.0-or-later
//! Whether a client of the agent's listeners is in a container.
//!
//! A container has its own network namespace: its connections reach the agent forwarded (the
//! capture rules redirect them in `prerouting`) or on the bridge's address, with a source address
//! from the bridge's subnet. The guest's own processes have the address of the interface they left
//! on, or loopback. So a source that none of the guest's interfaces has is a container's.
//! A container that shares the guest's network (`--network host`), one reaching the agent over IPv6,
//! and a rootless one whose traffic arrives from loopback (slirp4netns) are not told apart: they
//! look like the guest's own clients.
//!
//! The proxy gets the answer as [`puddle_agent_proto::origin::ORIGIN_HEADER`] on the request.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

/// Whether `peer` is a client in a container, by the guest's interface addresses. A listing that
/// fails counts every IPv4 peer but loopback as a container's: that costs decryption only, where
/// decrypting a container's connection would make its tools fail.
#[must_use]
pub fn from_container(peer: SocketAddr) -> bool {
    from_container_by(peer, &crate::ifaddrs::Getifaddrs)
}

/// [`from_container`] with the interface listing given.
fn from_container_by(peer: SocketAddr, interfaces: &impl crate::bridge::Interfaces) -> bool {
    match interfaces.ipv4() {
        Ok(all) => {
            let own: Vec<Ipv4Addr> = all.into_iter().map(|i| i.addr).collect();
            is_foreign(peer, &own)
        }
        Err(err) => {
            tracing::debug!(error = %err, "listing the guest's addresses failed; connections count as containers'");
            peer.is_ipv4() && !peer.ip().is_loopback()
        }
    }
}

/// A source that is an IPv4 address none of `own` is, and not loopback.
fn is_foreign(peer: SocketAddr, own: &[Ipv4Addr]) -> bool {
    match peer.ip() {
        IpAddr::V4(ip) => !ip.is_loopback() && !own.contains(&ip),
        IpAddr::V6(_) => false,
    }
}

#[cfg(test)]
mod tests {
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
}
