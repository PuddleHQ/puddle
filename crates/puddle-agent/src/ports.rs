// SPDX-License-Identifier: GPL-3.0-or-later
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
mod tests {
    use std::fmt::Write as _;
    use std::time::Duration;

    use puddle_test_kit::no_event_for;

    use super::*;

    const HEADER: &str = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n";

    fn row(local: &str, state: &str) -> String {
        format!(
            "   0: {local} 00000000:0000 {state} 00000000:00000000 00:00000000 00000000     0        0 1 1 0\n"
        )
    }

    /// The kernel's text for an address on this machine's byte order.
    fn v4(ip: [u8; 4], port: u16) -> String {
        format!("{:08X}:{port:04X}", u32::from_ne_bytes(ip))
    }

    fn v6(ip: [u8; 16], port: u16) -> String {
        let mut text = String::new();
        for word in ip.as_chunks::<4>().0 {
            write!(text, "{:08X}", u32::from_ne_bytes(*word)).unwrap();
        }
        format!("{text}:{port:04X}")
    }

    #[test]
    fn only_listeners_on_the_wildcard_or_loopback_are_ports_a_forward_can_reach() {
        let mut v6_loopback = [0u8; 16];
        v6_loopback[15] = 1;
        let mut mapped = [0u8; 16];
        mapped[10] = 0xff;
        mapped[11] = 0xff;
        mapped[12..].copy_from_slice(&[127, 0, 0, 1]);
        let mut other = [0u8; 16];
        other[0] = 0xfe;
        other[1] = 0x80;
        other[15] = 1;
        let text = [
            HEADER.to_owned(),
            row(&v4([0, 0, 0, 0], 3000), "0A"),
            row(&v4([127, 0, 0, 1], 8080), "0A"),
            row(&v4([127, 0, 0, 5], 8081), "0A"),
            row(&v4([172, 17, 0, 1], 9000), "0A"),
            row(&v4([0, 0, 0, 0], 9001), "01"),
            row(&v6([0; 16], 4000), "0A"),
            row(&v6(v6_loopback, 4001), "0A"),
            row(&v6(mapped, 4002), "0A"),
            row(&v6(other, 4003), "0A"),
            row(&v4([0, 0, 0, 0], 0), "0A"),
        ]
        .concat();
        let mut ports = parse_table(&text);
        ports.sort_unstable();
        assert_eq!(ports, [3000, 4000, 4001, 4002, 8080, 8081]);
    }

    #[test]
    fn rows_the_kernel_would_not_write_are_skipped() {
        let text = [
            HEADER,
            "garbage\n",
            "0: 0100007F 00000000:0000 0A\n",
            "0: 0100007F:ZZZZ 00000000:0000 0A x\n",
            "0: 0100007F:0050 00000000:0000\n",
            "0: 0100:0050 00000000:0000 0A x\n",
            "0: 0100007G:0050 00000000:0000 0A x\n",
            "0: 000000000000000000000000000000ZZ:0050 00000000:0000 0A x\n",
            "\u{0}\u{1}\n",
        ]
        .concat();
        assert_eq!(parse_table(&text), Vec::<u16>::new());
        assert_eq!(parse_table(""), Vec::<u16>::new());
    }

    #[test]
    fn the_agents_own_ports_are_left_out_and_a_missing_ipv6_table_is_fine() {
        let tmp = tempfile::Builder::new()
            .prefix("puddle-ports-read-")
            .tempdir()
            .unwrap();
        let dir = tmp.path().to_path_buf();
        std::fs::write(
            dir.join("tcp"),
            [
                HEADER.to_owned(),
                row(&v4([0; 4], 3128), "0A"),
                row(&v4([0; 4], 3000), "0A"),
            ]
            .concat(),
        )
        .unwrap();
        assert_eq!(read(&dir, &[3128]).unwrap(), BTreeSet::from([3000]));
        std::fs::remove_dir_all(&dir).unwrap();
        let err = read(&dir, &[]).unwrap_err();
        assert!(err.to_string().contains("puddle-ports-read"), "{err}");
    }

    #[tokio::test(start_paused = true)]
    async fn the_set_is_sent_at_start_and_again_only_when_it_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        let write = |ports: &[u16]| {
            let rows: String = ports.iter().map(|p| row(&v4([0; 4], *p), "0A")).collect();
            std::fs::write(dir.join("tcp"), format!("{HEADER}{rows}")).unwrap();
        };
        write(&[]);
        let (tx, mut rx) = mpsc::channel(8);
        let task = tokio::spawn(watch(
            PortWatch {
                dir: dir.clone(),
                poll: Duration::from_millis(100),
            },
            vec![],
            tx,
        ));
        assert_eq!(rx.recv().await, Some(AgentMessage::ports([])));
        // Nothing changes: nothing is sent.
        no_event_for(
            "a report while nothing changes",
            Duration::from_millis(500),
            &mut rx,
        )
        .await;
        write(&[8080, 3000]);
        assert_eq!(rx.recv().await, Some(AgentMessage::ports([3000, 8080])));
        write(&[3000]);
        assert_eq!(rx.recv().await, Some(AgentMessage::ports([3000])));
        // Unreadable tables say nothing and the watch carries on when they come back.
        std::fs::remove_file(dir.join("tcp")).unwrap();
        no_event_for(
            "a report while the table is unreadable",
            Duration::from_millis(300),
            &mut rx,
        )
        .await;
        write(&[3000, 5000]);
        assert_eq!(rx.recv().await, Some(AgentMessage::ports([3000, 5000])));
        drop(rx);
        task.await.unwrap();
    }
}
