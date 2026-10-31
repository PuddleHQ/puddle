// SPDX-License-Identifier: GPL-3.0-or-later
//! Hostile-guest cases at tier V that need only the msb adapter: a root process in the guest
//! tries to reach the network and every vsock address and port, and to make the host attach a
//! file of its choosing as its root disk. Each test boots a real microVM, so they run only under
//! the VM profile (`vm_` prefix).
mod support;

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use puddle_compute::{ComputeError, ExecRequest, Runtime, Sandbox, VsockRoute};
use puddle_ipc::IpcRoot;
use support::{cleanup, collect_lines, sh, spec, vsock_sweep};

/// The guest port Puddle routes.
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
/// the guest sends arrives on Puddle's endpoint; no port of CID 0 (the hypervisor), 1 (the local
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

/// The guest-side view of msb's root-disk journal: msb shares the sandbox's runtime folder with
/// the guest, writable, at this path.
const GUEST_JOURNAL: &str = "/.msb/root-disk.json";

/// A SHA-256 over a file's content: its length, then every 1 MiB piece that is not all zeros,
/// with its offset. Two files have the same one exactly when their bytes are the same. A root
/// disk is 4 GiB and mostly holes, and a debug build hashes at about 20 MB/s, so the zero
/// pieces (found by a plain comparison) are left out of the hash, not out of the comparison.
#[expect(
    clippy::unwrap_used,
    reason = "a file the test cannot read fails the test"
)]
fn fingerprint(path: &Path) -> String {
    use sha2::{Digest, Sha256};
    const PIECE: usize = 1 << 20;
    let zeros = vec![0_u8; PIECE];
    let mut file = std::fs::File::open(path).unwrap();
    let mut hasher = Sha256::new();
    hasher.update(file.metadata().unwrap().len().to_le_bytes());
    let mut buf = vec![0_u8; PIECE];
    let mut offset = 0_u64;
    loop {
        let n = read_piece(&mut file, &mut buf);
        let Some(piece) = buf.get(..n).filter(|p| !p.is_empty()) else {
            break;
        };
        if zeros.get(..n) != Some(piece) {
            hasher.update(offset.to_le_bytes());
            hasher.update(piece);
        }
        offset += n as u64;
    }
    support::mount_attack::hex(&hasher.finalize())
}

/// Fills `buf` from `file` unless the file ends first, so pieces start at fixed offsets.
#[expect(
    clippy::unwrap_used,
    reason = "a file the test cannot read fails the test"
)]
fn read_piece(file: &mut std::fs::File, buf: &mut [u8]) -> usize {
    let mut filled = 0;
    while let Some(rest) = buf.get_mut(filled..).filter(|r| !r.is_empty()) {
        let n = file.read(rest).unwrap();
        if n == 0 {
            break;
        }
        filled += n;
    }
    filled
}

/// As root in the guest, replaces the journal's first layer with `target` and its launch base
/// with the sandbox's own disk (`own`), which is what msb compares with the sandbox's config.
#[expect(
    clippy::indexing_slicing,
    clippy::expect_used,
    reason = "a journal not in msb's shape, or a failed exec, fails the test"
)]
async fn forge_journal<S: Sandbox>(sb: &S, journal: &serde_json::Value, own: &Path, target: &Path) {
    let mut forged = journal.clone();
    forged["layers"][0]["path"] = serde_json::json!(target);
    forged["launch_base"] = serde_json::json!(own);
    let out = sb
        .exec(
            ExecRequest::new(
                "sh",
                [
                    "-c".to_owned(),
                    format!(r#"printf '%s' "$1" > {GUEST_JOURNAL}"#),
                    "sh".to_owned(),
                    forged.to_string(),
                ],
            )
            .as_user("root"),
        )
        .await
        .expect("exec");
    assert_eq!(
        out.status.code,
        0,
        "the guest could not write the journal: {}",
        out.stderr_text()
    );
}

/// Guest root rewrites msb's root-disk journal so that the next start would attach another
/// workspace's disk, and then a host file, as its writable root disk. After each stop the start
/// is refused with the adapter's named problem, and both files hash the same as before. Between
/// the two, the journal msb wrote is put back and the start goes through: a stopped and started
/// sandbox with its own journal is not refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn vm_hostile_hg44_a_rewritten_root_disk_journal_attaches_no_file_outside_the_sandbox() {
    let settings = support::settings();
    let rt = support::runtime(&settings).await;
    let other = spec(&settings, "hg44-other");
    let other_name = other.name.clone();
    let s = spec(&settings, "hg44");
    let n = s.name.clone();
    let host_file = settings.home().join("hg44-host-file");
    std::fs::write(&host_file, b"a host file no guest may write\n").unwrap();

    let o = rt.create(other).await.expect("create the other workspace");
    o.stop().await.unwrap();
    let mut sb = rt.create(s).await.expect("create");
    let read = sh(&sb, &format!("cat {GUEST_JOURNAL}")).await;
    assert_eq!(
        read.status.code,
        0,
        "no journal in the guest: {}",
        read.stderr_text()
    );
    let journal: serde_json::Value = serde_json::from_str(&read.stdout_text()).unwrap();
    let own = PathBuf::from(journal["layers"][0]["path"].as_str().unwrap());
    let sandbox_dir = own.parent().unwrap().to_path_buf();
    let host_journal = sandbox_dir.join("runtime").join("root-disk.json");
    let original = std::fs::read(&host_journal).unwrap();
    let other_disk = sandbox_dir
        .parent()
        .unwrap()
        .join(other_name.as_str())
        .join("upper.ext4");
    assert!(other_disk.is_file(), "no disk at {}", other_disk.display());

    let mut outcomes = Vec::new();
    for (round, target) in [&other_disk, &host_file].into_iter().enumerate() {
        if round > 0 {
            // The journal msb wrote, back in place: the start must go through.
            std::fs::write(&host_journal, &original).unwrap();
            sb = rt.start(&n).await.expect("a start with msb's own journal");
        }
        forge_journal(&sb, &journal, &own, target).await;
        sb.stop().await.unwrap();
        let landed = std::fs::read_to_string(&host_journal).unwrap();
        let before = fingerprint(target);
        let started = rt.start(&n).await;
        // A start that went through has the file attached: stop it before reading the file
        // again, so what the guest wrote is on disk and the next round starts from a stopped
        // sandbox.
        let refused = match started {
            Ok(attached) => {
                attached.stop().await.unwrap();
                None
            }
            Err(e) => Some(e),
        };
        let after = fingerprint(target);
        outcomes.push((target.clone(), landed, refused, before, after));
    }
    cleanup(&rt, &[&n, &other_name]).await;
    // The run's home is removed with the run; this only tidies early.
    let _ = std::fs::remove_file(&host_file);
    for (target, landed, refused, before, after) in outcomes {
        let target_json = serde_json::json!(target).to_string();
        assert!(
            landed.contains(&target_json),
            "the guest's journal did not reach the host: {landed}"
        );
        match refused {
            Some(ComputeError::Runtime {
                op: "start",
                message,
            }) if message.contains("outside the sandbox's folder") => {}
            other => panic!(
                "the start with a journal naming {} was not refused by the check: {other:?}",
                target.display()
            ),
        }
        assert_eq!(before, after, "{} changed", target.display());
    }
}
