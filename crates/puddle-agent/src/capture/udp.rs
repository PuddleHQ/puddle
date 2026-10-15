// SPDX-License-Identifier: GPL-3.0-or-later
//! Tells the host about UDP the rules refused.
//!
//! The refusal itself is the kernel's (an `icmpx port-unreachable`, so the sender fails in well
//! under a second). The rules also copy each refused datagram's headers to a netfilter log group,
//! rate-limited in the kernel; this module reads that group and reports each distinct
//! destination, with a count, at most once per [`QUIET`] and never more than [`PER_SECOND`] reports
//! a second, so a guest spraying datagrams can neither hide behind the limit nor flood the host.

use std::collections::HashMap;
use std::net::IpAddr;
use std::time::{Duration, Instant};

use puddle_agent_proto::AgentMessage;

/// The same destination is reported again only after this long; refusals in between are counted.
pub const QUIET: Duration = Duration::from_secs(10);
/// Most reports sent per second.
pub const PER_SECOND: usize = 5;
/// Most distinct destinations tracked at once.
const TRACKED: usize = 1024;

/// Decides which refused datagrams become reports.
#[derive(Debug, Default)]
pub struct Reporter {
    seen: HashMap<(IpAddr, u16), Entry>,
    window_start: Option<Instant>,
    sent_in_window: usize,
}

#[derive(Debug)]
struct Entry {
    last_report: Option<Instant>,
    unreported: u32,
}

impl Reporter {
    /// Notes one refused datagram; returns the report to send now, if one is due.
    pub fn refused(&mut self, now: Instant, dest: IpAddr, port: u16) -> Option<AgentMessage> {
        if self.seen.len() >= TRACKED && !self.seen.contains_key(&(dest, port)) {
            // Full: forget everything not reported within the quiet period. That is at most a
            // few dozen entries, since reports are rate-limited.
            self.seen.retain(|_, e| {
                e.last_report
                    .is_some_and(|at| now.saturating_duration_since(at) < QUIET)
            });
        }
        let entry = self.seen.entry((dest, port)).or_insert(Entry {
            last_report: None,
            unreported: 0,
        });
        entry.unreported = entry.unreported.saturating_add(1);
        let due = entry
            .last_report
            .is_none_or(|at| now.saturating_duration_since(at) >= QUIET);
        if !due {
            return None;
        }
        if self
            .window_start
            .is_none_or(|start| now.saturating_duration_since(start) >= Duration::from_secs(1))
        {
            self.window_start = Some(now);
            self.sent_in_window = 0;
        }
        if self.sent_in_window >= PER_SECOND {
            return None;
        }
        self.sent_in_window += 1;
        let count = std::mem::take(&mut entry.unreported);
        entry.last_report = Some(now);
        Some(AgentMessage::udp_refused(dest.to_string(), port, count))
    }
}

/// Reads the log group and reports until the agent stops.
#[cfg(target_os = "linux")]
pub async fn watch(group: u16, reports: tokio::sync::mpsc::Sender<AgentMessage>) {
    use tokio::sync::mpsc;

    use super::{netlink, nft};
    let (tx, mut rx) = mpsc::channel(256);
    let reader = tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        let socket = netlink::subscribe_nflog(group, 64)?;
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            match socket.read(buf.as_mut_slice()) {
                Ok(len) => {
                    for item in nft::logged(buf.get(..len).unwrap_or_default()) {
                        // Over capacity the datagram is only not counted; it was refused anyway.
                        let _ = tx.try_send(item);
                    }
                }
                Err(err)
                    if matches!(
                        err.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    if tx.is_closed() {
                        return Ok(());
                    }
                }
                Err(err) => return Err(err),
            }
        }
    });
    let mut reporter = Reporter::default();
    while let Some((dest, proto, port)) = rx.recv().await {
        if proto != 17 {
            continue;
        }
        if let Some(report) = reporter.refused(Instant::now(), dest, port) {
            let _ = reports.try_send(report);
        }
    }
    match reader.await {
        Ok(Err(err)) => tracing::warn!(error = %err, "UDP refusals are no longer reported"),
        Ok(Ok(())) => {}
        Err(err) => tracing::warn!(error = %err, "UDP refusal reader stopped"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(last: u8) -> IpAddr {
        IpAddr::from([93, 184, 216, last])
    }

    #[test]
    fn the_first_datagram_to_a_destination_is_reported_and_repeats_are_counted() {
        let mut r = Reporter::default();
        let t0 = Instant::now();
        assert_eq!(
            r.refused(t0, ip(1), 443),
            Some(AgentMessage::udp_refused("93.184.216.1".into(), 443, 1))
        );
        for i in 1..=4 {
            assert_eq!(r.refused(t0 + Duration::from_secs(i), ip(1), 443), None);
        }
        let after = r.refused(t0 + QUIET, ip(1), 443);
        assert_eq!(
            after,
            Some(AgentMessage::udp_refused("93.184.216.1".into(), 443, 5))
        );
    }

    #[test]
    fn another_port_or_address_is_its_own_destination() {
        let mut r = Reporter::default();
        let t0 = Instant::now();
        assert!(r.refused(t0, ip(1), 443).is_some());
        assert!(r.refused(t0, ip(1), 123).is_some());
        assert!(r.refused(t0, ip(2), 443).is_some());
    }

    #[test]
    fn a_spray_of_destinations_is_cut_to_a_few_reports_a_second() {
        let mut r = Reporter::default();
        let t0 = Instant::now();
        let sent = (0..200u8)
            .filter(|i| r.refused(t0, ip(*i), 9).is_some())
            .count();
        assert_eq!(sent, PER_SECOND);
        // The next second reports the next ones; the count of the unreported is not lost.
        let later = t0 + Duration::from_secs(1);
        assert!(r.refused(later, ip(250), 9).is_some());
    }

    #[test]
    fn the_table_of_destinations_stays_bounded() {
        let mut r = Reporter::default();
        let t0 = Instant::now();
        for i in 0..(u32::try_from(TRACKED).unwrap() + 500) {
            let dest = IpAddr::from(i.to_be_bytes());
            let _ = r.refused(t0, dest, 1);
        }
        assert!(r.seen.len() <= TRACKED);
    }
}
