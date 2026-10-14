// SPDX-License-Identifier: GPL-3.0-or-later
//! Hostile-guest cases at tier V that need only the msb adapter: a root process in the guest
//! tries to reach the network (HG-01) and every vsock address and port (HG-02). Each test boots
//! a real microVM, so they run only under the VM profile (`vm_` prefix).
mod support;

use std::time::Duration;

use puddle_compute::{ExecRequest, Runtime, Sandbox, VsockRoute};
use puddle_ipc::IpcRoot;
use support::{cleanup, collect_lines, sh, spec, vsock_sweep};

/// The guest port puddle routes.
const ROUTE_PORT: u32 = 5000;

/// The host's vsock address.
const HOST_CID: u32 = 2;

/// Ports above the 16-bit range, which a vsock address has room for.
const HIGH_PORTS: [&str; 5] = ["65536", "100000", "1048576", "2147483648", "4294967294"];

/// Sends one ICMP echo request from a raw socket to each address given and waits 3 s for the
/// echo reply. Prints `<address> reply`, `<address> no-reply` or `<address> <step> failed: <errno>`.
const ICMP_PROBE: &str = r#"
use strict; use warnings; use Socket;
# usage: icmp.pl ADDRESS... ; prints "ADDRESS reply", "ADDRESS no-reply" or "ADDRESS <step> failed: <errno>"
sub checksum {
  my $data = shift;
  $data .= "\0" if length($data) % 2;
  my $sum = 0;
  $sum += $_ for unpack("n*", $data);
  $sum = ($sum >> 16) + ($sum & 0xffff);
  $sum += $sum >> 16;
  return ~$sum & 0xffff;
}
my $id = $$ & 0xffff;
my $body = pack("a32", "puddle-hg01");
my $packet = pack("CCnnn", 8, 0, checksum(pack("CCnnn", 8, 0, 0, $id, 1) . $body), $id, 1) . $body;
for my $dst (@ARGV) {
  socket(my $s, AF_INET, SOCK_RAW, 1) or do { print "$dst socket failed: $!\n"; next };
  send($s, $packet, 0, pack_sockaddr_in(0, inet_aton($dst))) or do { print "$dst send failed: $!\n"; next };
  my $reply = 0;
  my $until = time + 3;
  while (!$reply && time < $until) {
    my $readers = '';
    vec($readers, fileno($s), 1) = 1;
    next if select($readers, undef, undef, 1) <= 0;
    recv($s, my $buf, 2048, 0) or next;
    # A raw ICMP socket hands over the IP header too.
    my $ihl = (ord(substr($buf, 0, 1)) & 15) * 4;
    my ($type, undef, undef, $rid) = unpack("CCnn", substr($buf, $ihl, 6));
    $reply = 1 if $type == 0 && $rid == $id;
  }
  print "$dst ", ($reply ? "reply" : "no-reply"), "\n";
}
"#;

/// Direct connections from the guest all fail: no interface is up but loopback (msb's guest
/// kernel has a `dummy0` sink device, down), and DNS, IPv4 TCP, UDP and IPv6 reach nothing.
/// Loopback works (the agent listens on 127.0.0.1). A root process's raw ICMP echo requests get
/// no reply from three public addresses, while the same probe is answered by loopback (the proof
/// that the probe works), and raw sockets cannot be used to get around the missing interface.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn vm_hostile_hg01_guest_has_no_direct_network() {
    let settings = support::settings();
    let rt = support::runtime(&settings).await;
    let s = spec(&settings, "hg01");
    let n = s.name.clone();
    let sb = rt.create(s).await.expect("create");
    let script = r#"
fail=0
for i in /sys/class/net/*; do
  n=${i##*/}; [ "$n" = lo ] && continue
  # Only loopback may be up; the kernel's dummy0 (a sink device) may exist, down.
  flags=$(cat "$i/flags"); [ $((flags & 1)) -eq 0 ] || { echo "interface $n is up"; fail=1; }
done
timeout 5 getent hosts github.com && { echo "DNS resolved"; fail=1; }
timeout 5 bash -c 'echo > /dev/tcp/140.82.121.4/443' && { echo "direct IPv4 TCP connected"; fail=1; }
timeout 5 bash -c 'echo > /dev/udp/1.1.1.1/53' && { echo "direct UDP sent"; fail=1; }
timeout 5 bash -c 'echo > /dev/tcp/2606:4700:4700::1111/443' && { echo "direct IPv6 connected"; fail=1; }
out=$(timeout 5 bash -c 'echo > /dev/tcp/127.0.0.1/1' 2>&1)
case "$out" in *refused*) ;; *) echo "loopback unusable: $out"; fail=1 ;; esac
exit $fail
"#;
    let out = sh(&sb, script).await;
    eprintln!("{}{}", out.stdout_text(), out.stderr_text());
    let icmp = sb
        .exec(
            ExecRequest::new(
                "perl",
                [
                    "-e",
                    ICMP_PROBE,
                    "127.0.0.1",
                    "1.1.1.1",
                    "8.8.8.8",
                    "9.9.9.9",
                ],
            )
            .as_user("root")
            .with_timeout(Duration::from_secs(60)),
        )
        .await
        .expect("icmp probe");
    eprintln!("{}{}", icmp.stdout_text(), icmp.stderr_text());
    sb.stop().await.unwrap();
    cleanup(&rt, &[&n]).await;
    assert_eq!(out.status.code, 0, "{}", out.stdout_text());
    assert_eq!(icmp.status.code, 0, "{}", icmp.stderr_text());
    let icmp = icmp.stdout_text();
    let verdict = |address: &str| {
        icmp.lines()
            .find_map(|l| l.strip_prefix(address)?.strip_prefix(' '))
            .unwrap_or_else(|| panic!("no verdict for {address}: {icmp}"))
    };
    // Loopback must answer, unless the guest refuses raw sockets to root altogether: then nothing
    // can leave through one either, and there is nothing to compare against.
    let control = verdict("127.0.0.1");
    assert!(
        control == "reply" || control.starts_with("socket failed"),
        "the probe must be answered by loopback or have no raw socket: {icmp}"
    );
    for address in ["1.1.1.1", "8.8.8.8", "9.9.9.9"] {
        let v = verdict(address);
        assert!(
            v == "no-reply" || v.contains("failed"),
            "{address} answered a raw ICMP echo: {icmp}"
        );
    }
}

/// Of a connect sweep over every guest-to-host vsock port, only the routed one accepts, and what
/// the guest sends arrives on puddle's endpoint; no port of CID 0 (the hypervisor), 1 (the local
/// address) or 3 (a neighbour or the guest itself) accepts either.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn vm_hostile_hg02_only_the_routed_vsock_port_accepts() {
    let settings = support::settings();
    let rt = support::runtime(&settings).await;
    let root = IpcRoot::new().unwrap();
    let listener = root.listen().unwrap();
    let s =
        spec(&settings, "hg02").with_route(VsockRoute::new(ROUTE_PORT, listener.endpoint().path()));
    let n = s.name.clone();
    let lines = collect_lines(listener);
    let sb = rt.create(s).await.expect("create");
    let mut ranges = vec!["1-65535"];
    ranges.extend(HIGH_PORTS);
    let ranges = &ranges[..];
    let (host, hypervisor, local, neighbour) = tokio::join!(
        vsock_sweep(&sb, HOST_CID, "hello-hg02", ranges),
        vsock_sweep(&sb, 0, "hello-hg02-cid0", ranges),
        vsock_sweep(&sb, 1, "hello-hg02-cid1", ranges),
        vsock_sweep(&sb, 3, "hello-hg02-cid3", ranges),
    );
    sb.stop().await.unwrap();
    cleanup(&rt, &[&n]).await;
    for sweep in [&host, &hypervisor, &local, &neighbour] {
        eprintln!("{}", sweep.summary);
    }
    assert_eq!(host.accepted, [ROUTE_PORT], "host ports that accepted");
    for (cid, sweep) in [(0, hypervisor), (1, local), (3, neighbour)] {
        assert!(
            sweep.accepted.is_empty(),
            "CID {cid} accepted ports {:?}: {}",
            sweep.accepted,
            sweep.summary
        );
    }
    assert_eq!(lines.await.unwrap(), ["hello-hg02"]);
}
