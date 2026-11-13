// SPDX-License-Identifier: AGPL-3.0-or-later
//! The interface the stand-in range lives on, and which interface counts as "outbound".
//!
//! A connect to a stand-in address needs a route before the redirect can act, so the agent keeps a
//! `dummy0` with the stub's address (`198.18.0.1/15`, which also routes the range). The guest's
//! default route normally leaves by the network device the VM runtime attached (with its own
//! deny policy); when there is none, the agent adds one through `dummy0`, so a connect to any
//! address reaches the redirect instead of failing with "network unreachable".
//!
//! Everything is idempotent and goes through netlink: no `ip` binary exists in the image.

use std::io;
use std::net::Ipv4Addr;

use super::netlink::{F_ACK, F_CREATE, F_EXCL, F_REPLACE, F_REQUEST, Socket, Writer};

const RTM_NEWLINK: u16 = 16;
const RTM_NEWADDR: u16 = 20;
const RTM_NEWROUTE: u16 = 24;
const IFLA_IFNAME: u16 = 3;
const IFLA_LINKINFO: u16 = 18;
const IFLA_INFO_KIND: u16 = 1;
const IFA_ADDRESS: u16 = 1;
const IFA_LOCAL: u16 = 2;
const RTA_OUT_INTERFACE: u16 = 4;
const AF_INET: u8 = 2;
const IFF_UP: u32 = 1;
const EEXIST: i32 = 17;

/// The interface of the default route in the text of `/proc/net/route`: the one that is up with
/// the lowest metric. `None` when there is no default route.
#[must_use]
pub fn default_route_interface(table: &str) -> Option<String> {
    table
        .lines()
        .skip(1)
        .filter_map(|line| {
            let f: Vec<&str> = line.split_whitespace().collect();
            let (name, dest, flags, metric, mask) =
                (f.first()?, f.get(1)?, f.get(3)?, f.get(6)?, f.get(7)?);
            let up = u32::from_str_radix(flags, 16).ok()? & 1 == 1;
            let default = u32::from_str_radix(dest, 16).ok()? == 0
                && u32::from_str_radix(mask, 16).ok()? == 0;
            (up && default).then(|| {
                (
                    metric.parse::<u32>().unwrap_or(u32::MAX),
                    (*name).to_owned(),
                )
            })
        })
        .min()
        .map(|(_, name)| name)
}

fn ifinfo(index: i32, flags: u32, change: u32) -> [u8; 16] {
    let mut out = [0u8; 16];
    out[4..8].copy_from_slice(&index.to_ne_bytes());
    out[8..12].copy_from_slice(&flags.to_ne_bytes());
    out[12..16].copy_from_slice(&change.to_ne_bytes());
    out
}

/// Makes `name` a dummy interface (an existing one is kept), brings it up and gives it
/// `address/prefix`; then returns the name of the interface the default route leaves by, adding a
/// default route through `name` when there is none.
///
/// Blocking: call from `spawn_blocking`.
///
/// # Errors
///
/// A netlink failure (no `CAP_NET_ADMIN`, no dummy driver in the kernel).
pub fn setup(name: &str, address: Ipv4Addr, prefix: u8) -> io::Result<String> {
    let socket = Socket::route()?;
    let mut w = Writer::default();
    let at = w.begin(RTM_NEWLINK, F_REQUEST | F_ACK | F_CREATE | F_EXCL, 1);
    w.raw(&ifinfo(0, 0, 0));
    w.string(IFLA_IFNAME, name);
    w.nested(IFLA_LINKINFO, |w| w.string(IFLA_INFO_KIND, "dummy"));
    w.end(at);
    match socket.transact(&w.finish(), 1) {
        Ok(()) => {}
        Err(err) if err.raw_os_error() == Some(EEXIST) => {}
        Err(err) => return Err(err),
    }
    let index = socket.interface_index(name)?;
    let mut w = Writer::default();
    let up = w.begin(RTM_NEWLINK, F_REQUEST | F_ACK, 2);
    w.raw(&ifinfo(i32::try_from(index).unwrap_or(0), IFF_UP, IFF_UP));
    w.end(up);
    let addr = w.begin(RTM_NEWADDR, F_REQUEST | F_ACK | F_CREATE | F_REPLACE, 3);
    let mut body = [0u8; 8];
    body[0] = AF_INET;
    body[1] = prefix;
    body[4..8].copy_from_slice(&index.to_ne_bytes());
    w.raw(&body);
    w.attr(IFA_LOCAL, &address.octets());
    w.attr(IFA_ADDRESS, &address.octets());
    w.end(addr);
    socket.transact(&w.finish(), 2)?;
    if let Some(wan) = default_route_interface(&std::fs::read_to_string("/proc/net/route")?) {
        return Ok(wan);
    }
    let mut w = Writer::default();
    let route = w.begin(RTM_NEWROUTE, F_REQUEST | F_ACK | F_CREATE | F_EXCL, 4);
    // family, dst_len 0 (default), src_len, tos, table main (254), proto boot (3), scope link (253),
    // type unicast (1), flags.
    w.raw(&[AF_INET, 0, 0, 0, 254, 3, 253, 1, 0, 0, 0, 0]);
    w.attr(RTA_OUT_INTERFACE, &index.to_ne_bytes());
    w.end(route);
    match socket.transact(&w.finish(), 1) {
        Ok(()) => {}
        Err(err) if err.raw_os_error() == Some(EEXIST) => {}
        Err(err) => return Err(err),
    }
    Ok(name.to_owned())
}

#[cfg(test)]
mod tests;
