// SPDX-License-Identifier: GPL-3.0-or-later
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
mod tests {
    use super::*;
    use std::ffi::CStr;
    use std::net::Ipv4Addr;

    #[test]
    fn the_loopback_interface_is_listed_with_its_address() {
        let all = Getifaddrs.ipv4().unwrap();
        let lo = all
            .iter()
            .find(|i| i.addr == Ipv4Addr::LOCALHOST)
            .expect("127.0.0.1 is on some interface");
        assert!(lo.index > 0);
        assert_ne!(lo.name, "");
    }

    /// The raw `getifaddrs(3)` walk the agent used before it moved to `nix`, kept as the oracle.
    #[expect(unsafe_code, reason = "reference walk over libc's list, test only")]
    fn raw_ipv4() -> Vec<(String, u32, Ipv4Addr)> {
        let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
        // SAFETY: `head` is a valid out-pointer; the list is freed below.
        assert_eq!(unsafe { libc::getifaddrs(&raw mut head) }, 0);
        let mut out = Vec::new();
        let mut cursor = head;
        // SAFETY: `cursor` is null or a node of the live list.
        while let Some(e) = unsafe { cursor.as_ref() } {
            cursor = e.ifa_next;
            // SAFETY: the family field is only read when `ifa_addr` is non-null, so it is readable.
            let is_ipv4 = !e.ifa_addr.is_null()
                && libc::c_int::from(unsafe { (*e.ifa_addr).sa_family }) == libc::AF_INET;
            if !is_ipv4 {
                continue;
            }
            // SAFETY: AF_INET addresses are `sockaddr_in`; `ifa_name` is NUL-terminated.
            let (raw, name) = unsafe {
                (
                    (*e.ifa_addr.cast::<libc::sockaddr_in>()).sin_addr.s_addr,
                    CStr::from_ptr(e.ifa_name),
                )
            };
            // SAFETY: `name` is a valid C string.
            let index = unsafe { libc::if_nametoindex(name.as_ptr()) };
            if index != 0 {
                out.push((
                    name.to_string_lossy().into_owned(),
                    index,
                    Ipv4Addr::from(u32::from_be(raw)),
                ));
            }
        }
        // SAFETY: `head` came from the successful call above and is freed once.
        unsafe { libc::freeifaddrs(head) };
        out
    }

    #[test]
    fn the_list_matches_a_raw_getifaddrs_walk() {
        let mut got: Vec<_> = Getifaddrs
            .ipv4()
            .unwrap()
            .into_iter()
            .map(|i| (i.name, i.index, i.addr))
            .collect();
        let mut want = raw_ipv4();
        got.sort();
        want.sort();
        assert_eq!(got, want);
    }
}
