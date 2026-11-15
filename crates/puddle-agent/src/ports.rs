// SPDX-License-Identifier: AGPL-3.0-or-later
//! The listening-ports watch: which TCP ports do processes in the guest listen on?
//!
//! The kernel's `/proc/net/tcp` and `/proc/net/tcp6` list every TCP socket of the guest's network
//! namespace; a row in state `0A` is a listener. The watch reads both tables every
//! [`PortWatch::poll`](crate::config::PortWatch) and sends the whole set on the control stream
//! ([`AgentMessage::Ports`]) at start and whenever it changes. A poll rather than a netlink
//! subscription: the guest kernel's `sock_diag` is not guaranteed, the tables are always there, and
//! a port that lives for less than a second is not one a user can forward.
//!
//! Only listeners a connection to the guest's loopback reaches are reported: a socket bound to the
//! wildcard or to a loopback address. One bound to a single other address (the Docker bridge, an
//! interface of its own) would refuse the host's connection, so listing it would offer a forward
//! that cannot work. The agent's own ports (`Config::reserved_ports`) are left out.

use std::collections::BTreeSet;
use std::io;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::path::Path;

use puddle_agent_proto::AgentMessage;
use tokio::sync::mpsc;
use tokio::time::MissedTickBehavior;

use crate::config::PortWatch;

/// The `st` value of a listening socket in `/proc/net/tcp*`.
const LISTEN: &str = "0A";

/// The ports a table lists as listening on an address the guest's loopback reaches. A row that
/// doesn't parse is skipped: the format is the kernel's, but a surprise must not stop the watch.
///
/// ```
/// let table = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n   0: 00000000:0BB8 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 1 1 0\n   1: 0100007F:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 2 1 0\n   2: 0100007F:C350 0100007F:1F90 01 00000000:00000000 00:00000000 00000000     0        0 3 1 0\n";
/// assert_eq!(puddle_agent::ports::parse_table(table), [3000, 8080]);
/// ```
#[must_use]
pub fn parse_table(text: &str) -> Vec<u16> {
    text.lines()
        .skip(1)
        .filter_map(|row| {
            let mut fields = row.split_whitespace();
            let _slot = fields.next()?;
            let local = fields.next()?;
            let _remote = fields.next()?;
            (fields.next()? == LISTEN).then_some(())?;
            let (address, port) = local.rsplit_once(':')?;
            reachable_by_loopback(address).then_some(())?;
            u16::from_str_radix(port, 16).ok().filter(|p| *p != 0)
        })
        .collect()
}

/// Whether a socket bound to `hex` (the kernel's address text: one host-order word for IPv4, four
/// for IPv6) takes a connection to the guest's loopback.
fn reachable_by_loopback(hex: &str) -> bool {
    match hex.len() {
        8 => u32::from_str_radix(hex, 16).is_ok_and(|word| {
            // The kernel prints the address as the machine's native word, so on a
            // little-endian guest the first octet is the low byte.
            let ip = Ipv4Addr::from(word.to_ne_bytes());
            ip.is_unspecified() || ip.is_loopback()
        }),
        32 => {
            let mut octets = [0u8; 16];
            for (i, chunk) in octets.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                let Some(word) = hex
                    .get(i * 8..i * 8 + 8)
                    .and_then(|w| u32::from_str_radix(w, 16).ok())
                else {
                    return false;
                };
                chunk.copy_from_slice(&word.to_ne_bytes());
            }
            let ip = Ipv6Addr::from(octets);
            ip.is_unspecified()
                || ip.is_loopback()
                || ip
                    .to_ipv4_mapped()
                    .is_some_and(|v4| v4.is_unspecified() || v4.is_loopback())
        }
        _ => false,
    }
}

/// The ports listening now, from the tables in `dir`, without `reserved`. A table that can't be
/// read counts as empty (a guest without IPv6 has no `tcp6`); both unreadable is an error.
///
/// # Errors
///
/// Neither table could be read.
pub fn read(dir: &Path, reserved: &[u16]) -> io::Result<BTreeSet<u16>> {
    let v4 = std::fs::read_to_string(dir.join("tcp"));
    let v6 = std::fs::read_to_string(dir.join("tcp6"));
    if let (Err(err), Err(_)) = (&v4, &v6) {
        return Err(io::Error::new(
            err.kind(),
            format!("{}: {err}", dir.display()),
        ));
    }
    Ok(v4
        .into_iter()
        .chain(v6)
        .flat_map(|text| parse_table(&text))
        .filter(|port| !reserved.contains(port))
        .collect())
}

/// Reports the listening ports on `out`: once at start, then on every change, until `out` closes.
pub async fn watch(watch: PortWatch, reserved: Vec<u16>, out: mpsc::Sender<AgentMessage>) {
    let mut tick = tokio::time::interval(watch.poll);
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut last: Option<BTreeSet<u16>> = None;
    let mut failing = false;
    loop {
        tokio::select! {
            () = out.closed() => return,
            _ = tick.tick() => {}
        }
        let dir = watch.dir.clone();
        let reserved = reserved.clone();
        let now = match tokio::task::spawn_blocking(move || read(&dir, &reserved)).await {
            Ok(Ok(now)) => {
                failing = false;
                now
            }
            Ok(Err(err)) => {
                if !failing {
                    tracing::warn!(error = %err, "the guest's socket tables are unreadable: the Ports tab lists nothing");
                    failing = true;
                }
                continue;
            }
            Err(_) => return,
        };
        if last.as_ref() == Some(&now) {
            continue;
        }
        // A full queue keeps `last` as it was, so the next poll sends the set again.
        if out
            .try_send(AgentMessage::ports(now.iter().copied()))
            .is_ok()
        {
            last = Some(now);
        }
    }
}

#[cfg(test)]
mod tests;
