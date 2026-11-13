// SPDX-License-Identifier: GPL-3.0-or-later
//! The hostile-guest cases: a root process in the guest tries to reach the network and every
//! vsock address and port. They hold for any runtime, because the proxy is the only way out of a
//! sandbox only if the runtime leaves no other.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use puddle_ipc::IpcRoot;
use tokio::io::AsyncReadExt;
use tokio::task::JoinHandle;

use super::{Case, Outcome, sh, spec, step};
use crate::probes::{ICMP_PROBE, LOCAL_CID, NO_DIRECT_NETWORK, PACKET_PROBE, VSOCK_SWEEP};
use crate::{ExecOutput, ExecRequest, Runtime, Sandbox, VsockRoute};

/// The guest port Puddle routes.
const ROUTE_PORT: u32 = 5000;

/// The host's vsock address.
const HOST_CID: u32 = 2;

/// Ports above the 16-bit range, which a vsock address has room for.
const HIGH_PORTS: [&str; 5] = ["65536", "100000", "1048576", "2147483648", "4294967294"];

/// The vsock addresses the first sweep leaves out: other guests' CIDs, a high one, the largest
/// valid CIDs and `VMADDR_CID_ANY`.
const OTHER_CIDS: [u32; 7] = [4, 5, 6, 100, 0x7fff_ffff, 0xffff_fffe, 0xffff_ffff];

/// The ports tried on every other address: the low ones, the routed one with its neighbours,
/// the usual proxy and web ports and the ephemeral range's start.
const OTHER_CID_PORTS: [&str; 5] = ["1-1024", "3128", "4990-5010", "8080", "49152-49300"];

/// How long a probe may run past its own timeout before the case gives up on it.
const PROBE_SLACK: Duration = Duration::from_secs(60);

/// How long the host waits for a connection a sweep made to reach its endpoint.
const DELIVERY_WAIT: Duration = Duration::from_secs(10);

/// How long the host keeps listening after the expected connections came, to catch extra ones.
const EXTRA_WAIT: Duration = Duration::from_secs(1);

/// Runs a probe as root and returns its output.
async fn probe<S: Sandbox>(
    sb: &S,
    what: &str,
    program: &str,
    args: Vec<String>,
    timeout: Duration,
) -> Result<ExecOutput, String> {
    let request = ExecRequest::new(program, args)
        .as_user("root")
        .with_timeout(timeout);
    match tokio::time::timeout(timeout + PROBE_SLACK, sb.exec(request)).await {
        Ok(Ok(out)) => Ok(out),
        Ok(Err(e)) => Err(format!("{what}: unexpected error: {e}")),
        Err(_) => Err(format!("{what}: no answer within {timeout:?}")),
    }
}

async fn perl<S: Sandbox>(
    sb: &S,
    what: &str,
    script: &str,
    args: &[&str],
    timeout: Duration,
) -> Result<ExecOutput, String> {
    let mut all = vec!["-e".to_owned(), script.to_owned()];
    all.extend(args.iter().map(|a| (*a).to_owned()));
    probe(sb, what, "perl", all, timeout).await
}

/// Direct connections from the guest all fail: no interface is up but loopback, and DNS, IPv4
/// TCP, UDP and IPv6 reach nothing. Loopback works. A root process's raw ICMP echo requests get
/// no reply from three public addresses, while loopback answers the same probe (the proof that
/// the probe works).
pub(super) async fn hg01_guest_has_no_direct_network<R: Runtime>(c: &Case<'_, R>) -> Outcome {
    let sb = step("create", c.runtime().create(spec(c, &c.sandbox("a")?))).await?;
    let out = sh(&sb, NO_DIRECT_NETWORK).await?;
    check!(
        out.status.code == 0,
        "the guest reached out or lost loopback: {}{}",
        out.stdout_text(),
        out.stderr_text()
    );
    let icmp = perl(
        &sb,
        "icmp probe",
        ICMP_PROBE,
        &["127.0.0.1", "1.1.1.1", "8.8.8.8", "9.9.9.9"],
        Duration::from_secs(60),
    )
    .await?;
    check!(
        icmp.status.code == 0,
        "the icmp probe failed: {}",
        icmp.stderr_text()
    );
    let report = icmp.stdout_text();
    let verdict = |address: &str| {
        report
            .lines()
            .find_map(|l| l.strip_prefix(address)?.strip_prefix(' '))
            .ok_or_else(|| format!("no verdict for {address}: {report}"))
    };
    // Loopback must answer, unless the guest refuses raw sockets to root altogether: then nothing
    // can leave through one either, and there is nothing to compare against.
    let control = verdict("127.0.0.1")?;
    check!(
        control == "reply" || control.starts_with("socket failed"),
        "the probe must be answered by loopback or have no raw socket: {report}"
    );
    for address in ["1.1.1.1", "8.8.8.8", "9.9.9.9"] {
        let v = verdict(address)?;
        check!(
            v == "no-reply" || v.contains("failed"),
            "{address} answered a raw ICMP echo: {report}"
        );
    }
    Ok(())
}

/// Past the plain sockets of the first case: as root, raw IP sockets and packet sockets
/// (`AF_PACKET`, the way a tool writes its own frames) reach no network. A raw send to a public
/// address finds no route, and a frame on any device but loopback is refused; and even when
/// root gives the sink device an address and a default route, TCP, UDP and DNS to public
/// addresses reach nothing. The probe is proven by loopback: a second packet socket captures the
/// frame sent on it, and a raw send to 127.0.0.1 goes out.
pub(super) async fn hg01_raw_and_packet_sockets_reach_no_network<R: Runtime>(
    c: &Case<'_, R>,
) -> Outcome {
    let sb = step("create", c.runtime().create(spec(c, &c.sandbox("a")?))).await?;
    let out = perl(
        &sb,
        "packet probe",
        PACKET_PROBE,
        &[],
        Duration::from_secs(120),
    )
    .await?;
    let report = out.stdout_text();
    check!(
        out.status.code == 0,
        "the packet probe failed: {}",
        out.stderr_text()
    );
    let has = |line: &str| report.lines().any(|l| l == line);
    // The probe works where it may: loopback.
    check!(
        has("control-packet captured") || report.contains("control-packet socket failed"),
        "the packet probe is not proven by loopback: {report}"
    );
    check!(has("raw-control sent"), "raw control: {report}");
    // Nothing leaves: no raw send to a public address, no frame on a device but loopback.
    for l in report.lines() {
        let leaked = (l.starts_with("raw") && l.ends_with(" sent"))
            || (l.starts_with("packet ") && l.ends_with(" sent"))
            || l.ends_with("connected")
            || l.ends_with("answered");
        check!(
            !leaked || l == "raw-control sent",
            "the guest got out: {l}\n{report}"
        );
    }
    check!(
        !report.contains("setup failed"),
        "root could not set the sink device up, so phase two proved nothing: {report}"
    );
    for dst in ["140.82.121.4", "1.1.1.1"] {
        check!(
            report
                .lines()
                .any(|l| l.starts_with(&format!("tcp {dst} failed"))),
            "no TCP verdict for {dst}: {report}"
        );
    }
    check!(has("udp 1.1.1.1 no-answer"), "{report}");
    Ok(())
}

/// A host endpoint that collects the first line of every connection made to it.
struct HostEndpoint {
    path: PathBuf,
    lines: Arc<Mutex<Vec<String>>>,
    task: JoinHandle<()>,
}

impl HostEndpoint {
    fn start() -> Result<Self, String> {
        let root = IpcRoot::new().map_err(|e| e.to_string())?;
        let mut listener = root.listen().map_err(|e| e.to_string())?;
        let path = listener.endpoint().path().to_owned();
        let lines = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&lines);
        let task = tokio::spawn(async move {
            // The root's directory goes away with its last holder.
            let _root = root;
            while let Ok(mut conn) = listener.accept().await {
                let mut buf = Vec::new();
                let _ =
                    tokio::time::timeout(Duration::from_secs(5), conn.read_to_end(&mut buf)).await;
                let line = String::from_utf8_lossy(&buf).trim_end().to_owned();
                seen.lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(line);
            }
        });
        Ok(Self { path, lines, task })
    }

    fn seen(&self) -> Vec<String> {
        self.lines
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Waits for `expected` connections, then a little longer for any extra, and returns what
    /// arrived.
    async fn finish(&self, expected: usize) -> Vec<String> {
        let _ = tokio::time::timeout(DELIVERY_WAIT, async {
            while self.seen().len() < expected {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await;
        tokio::time::sleep(EXTRA_WAIT).await;
        self.seen()
    }
}

impl Drop for HostEndpoint {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// What a vsock sweep of one CID saw.
struct Sweep {
    /// The ports that accepted the connection, in the order found.
    accepted: Vec<u32>,
    /// How the rest ended, as the probe summarised it (for the failure message).
    summary: String,
}

/// Connects to every port in `ports` (numbers or `lo-hi` ranges) of vsock address `cid` from
/// inside the guest and sends `message` down each connection that is accepted.
async fn sweep<S: Sandbox>(
    sb: &S,
    cid: u32,
    message: &str,
    ports: &[&str],
) -> Result<Sweep, String> {
    let cid_arg = cid.to_string();
    let mut args = vec![message, cid_arg.as_str()];
    args.extend(ports);
    let out = perl(
        sb,
        &format!("vsock sweep of CID {cid}"),
        VSOCK_SWEEP,
        &args,
        Duration::from_secs(600),
    )
    .await?;
    check!(
        out.status.code == 0,
        "sweep of CID {cid} failed: {}",
        out.stderr_text()
    );
    let accepted = out
        .stdout_text()
        .lines()
        .map(|l| {
            l.strip_prefix("accepted ")
                .and_then(|p| p.parse().ok())
                .ok_or_else(|| format!("unreadable sweep line {l:?}"))
        })
        .collect::<Result<_, _>>()?;
    Ok(Sweep {
        accepted,
        summary: out.stderr_text().trim_end().to_owned(),
    })
}

/// Of a connect sweep over every guest-to-host vsock port, only the routed one accepts, and what
/// the guest sends arrives on Puddle's endpoint; no port of CID 0 (the hypervisor), 1 (the local
/// address) or 3 (a neighbour or the guest itself) accepts either.
pub(super) async fn hg02_only_the_routed_vsock_port_accepts<R: Runtime>(
    c: &Case<'_, R>,
) -> Outcome {
    let host = HostEndpoint::start()?;
    let s = spec(c, &c.sandbox("a")?).with_route(VsockRoute::new(ROUTE_PORT, host.path.clone()));
    let sb = step("create", c.runtime().create(s)).await?;
    let mut ranges = vec!["1-65535"];
    ranges.extend(HIGH_PORTS);
    let (on_host, hypervisor, local, neighbour) = tokio::join!(
        sweep(&sb, HOST_CID, "hello-hg02", &ranges),
        sweep(&sb, 0, "hello-hg02-cid0", &ranges),
        sweep(&sb, 1, "hello-hg02-cid1", &ranges),
        sweep(&sb, 3, "hello-hg02-cid3", &ranges),
    );
    let on_host = on_host?;
    check!(
        on_host.accepted == [ROUTE_PORT],
        "host ports that accepted: {:?}: {}",
        on_host.accepted,
        on_host.summary
    );
    for (cid, found) in [(0, hypervisor), (1, local), (3, neighbour)] {
        let found = found?;
        check!(
            found.accepted.is_empty(),
            "CID {cid} accepted ports {:?}: {}",
            found.accepted,
            found.summary
        );
    }
    let lines = host.finish(1).await;
    check!(
        lines == ["hello-hg02"],
        "the host endpoint received {lines:?}, want one \"hello-hg02\""
    );
    Ok(())
}

/// Past the sweep of the host and CIDs 0, 1 and 3: the routed port accepts on the host and on no
/// other address. Guests' CIDs, the largest valid CIDs, `VMADDR_CID_ANY` and the guest's own
/// CID refuse every port of the sweep, the routed one included, and the same probe is accepted
/// at the host's routed port in the same run (the proof that the probe encodes a CID that
/// connects).
pub(super) async fn hg02_no_other_vsock_address_accepts<R: Runtime>(c: &Case<'_, R>) -> Outcome {
    let host = HostEndpoint::start()?;
    let s = spec(c, &c.sandbox("a")?).with_route(VsockRoute::new(ROUTE_PORT, host.path.clone()));
    let sb = step("create", c.runtime().create(s)).await?;
    let own = perl(&sb, "local CID", LOCAL_CID, &[], Duration::from_secs(30)).await?;
    let own_cid: Option<u32> = own.stdout_text().trim().parse().ok();
    let control = sweep(&sb, HOST_CID, "hello-hg02-control", &["5000"]).await?;
    let cids: Vec<u32> = OTHER_CIDS.into_iter().chain(own_cid).collect();
    let mut sweeps = Vec::new();
    for cid in cids {
        let message = format!("hello-hg02-{cid}");
        sweeps.push((cid, sweep(&sb, cid, &message, &OTHER_CID_PORTS).await?));
    }
    check!(
        control.accepted == [ROUTE_PORT],
        "the control sweep of the host: {:?}: {}",
        control.accepted,
        control.summary
    );
    for (cid, found) in sweeps {
        check!(
            found.accepted.is_empty(),
            "CID {cid} accepted ports {:?}: {}",
            found.accepted,
            found.summary
        );
    }
    let lines = host.finish(1).await;
    check!(
        lines == ["hello-hg02-control"],
        "the host endpoint received {lines:?}, want only the control line"
    );
    Ok(())
}
