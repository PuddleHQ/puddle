// SPDX-License-Identifier: AGPL-3.0-or-later
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
mod tests;
