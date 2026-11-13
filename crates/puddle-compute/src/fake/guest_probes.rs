// SPDX-License-Identifier: GPL-3.0-or-later
//! The fake guest's answers to the hostile-guest probes of [`crate::probes`]: a guest with no
//! network, whose only vsock destination is the host and whose only open host ports are the
//! sandbox's routed ones.

use std::fmt::Write as _;

use tokio::io::AsyncWriteExt;

use crate::probes::Probe;
use crate::{ExecOutput, SandboxSpec};

/// The vsock address of the host.
const HOST_CID: u64 = 2;

/// The fake guest's own vsock address.
const GUEST_CID: u32 = 3;

/// What the fake guest answers to `probe`, called with the arguments after its script. Sends the
/// sweep's message down every route it reaches.
pub(super) fn answer(probe: Probe, args: &[&str], spec: &SandboxSpec) -> ExecOutput {
    match probe {
        Probe::NoDirectNetwork => ExecOutput::new(0, "", ""),
        Probe::Icmp => ExecOutput::new(0, icmp(args), ""),
        Probe::Packet => ExecOutput::new(0, PACKET_REPORT, ""),
        Probe::LocalCid => ExecOutput::new(0, format!("{GUEST_CID}\n"), ""),
        Probe::VsockSweep => sweep(args, spec),
    }
}

/// Loopback answers an echo request, nothing else does.
fn icmp(addresses: &[&str]) -> String {
    let mut report = String::new();
    for address in addresses {
        let verdict = if address.starts_with("127.") {
            "reply"
        } else {
            "no-reply"
        };
        // Writing to a String can't fail.
        let _ = writeln!(report, "{address} {verdict}");
    }
    report
}

/// What the packet probe prints in a guest that can reach only itself.
const PACKET_REPORT: &str = "\
devices: dummy0
control-packet captured
packet dummy0 send failed: Network is down
raw1 1.1.1.1 send failed: Network is unreachable
raw1 9.9.9.9 send failed: Network is unreachable
raw6 1.1.1.1 send failed: Network is unreachable
raw6 9.9.9.9 send failed: Network is unreachable
raw17 1.1.1.1 send failed: Network is unreachable
raw17 9.9.9.9 send failed: Network is unreachable
raw255 1.1.1.1 send failed: Network is unreachable
raw255 9.9.9.9 send failed: Network is unreachable
raw-control sent
tcp 140.82.121.4 failed: Network is unreachable
tcp 1.1.1.1 failed: Network is unreachable
udp 1.1.1.1 no-answer
";

/// Connects to every port in the ranges of CID `cid`: only the host's routed ports accept, and
/// each accepted connection carries the message to the route's host endpoint.
fn sweep(args: &[&str], spec: &SandboxSpec) -> ExecOutput {
    let [message, cid, ranges @ ..] = args else {
        return ExecOutput::new(2, "", "usage: sweep MESSAGE CID PORT|LO-HI ...\n");
    };
    let Ok(cid) = cid.parse::<u64>() else {
        return ExecOutput::new(2, "", format!("bad CID {cid}\n"));
    };
    let in_ranges = |port: u32| {
        ranges.iter().any(|r| {
            let (lo, hi) = r.split_once('-').unwrap_or((r, r));
            matches!(
                (lo.parse::<u64>(), hi.parse::<u64>()),
                (Ok(lo), Ok(hi)) if (lo..=hi).contains(&u64::from(port))
            )
        })
    };
    let mut routes: Vec<_> = spec
        .routes
        .iter()
        .filter(|r| cid == HOST_CID && in_ranges(r.guest_port))
        .collect();
    routes.sort_by_key(|r| r.guest_port);
    let mut accepted = String::new();
    for route in routes {
        // Writing to a String can't fail.
        let _ = writeln!(accepted, "accepted {}", route.guest_port);
        let host = route.host.clone();
        let line = format!("{message}\n");
        // The guest's connection outlives the exec; nobody waits for it.
        tokio::spawn(async move {
            if let Ok(mut conn) = puddle_ipc::connect(&host).await {
                let _ = conn.write_all(line.as_bytes()).await;
                let _ = conn.shutdown().await;
            }
        });
    }
    ExecOutput::new(0, accepted, format!("cid {cid}: probe done\n"))
}
