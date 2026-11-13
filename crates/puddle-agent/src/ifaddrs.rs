// SPDX-License-Identifier: AGPL-3.0-or-later
//! The guest's IPv4 interface addresses through `getifaddrs(3)` (Linux only). The bridge watcher
//! ([`crate::bridge`]) must know which interface owns an address, and std has no interface listing.

use std::io;

use nix::net::if_::if_nametoindex;
use nix::sys::socket::SockaddrIn;

use crate::bridge::{IfAddr, Interfaces};

/// The real interface list.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Getifaddrs;

impl Interfaces for Getifaddrs {
    fn ipv4(&self) -> io::Result<Vec<IfAddr>> {
        let mut out = Vec::new();
        for entry in nix::ifaddrs::getifaddrs()? {
            let Some(addr) = entry
                .address
                .and_then(|a| a.as_sockaddr_in().map(SockaddrIn::ip))
            else {
                continue;
            };
            // An interface that vanished between the two calls has no index; skip it.
            let Ok(index) = if_nametoindex(entry.interface_name.as_str()) else {
                continue;
            };
            out.push(IfAddr {
                name: entry.interface_name,
                index,
                addr,
            });
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests;
