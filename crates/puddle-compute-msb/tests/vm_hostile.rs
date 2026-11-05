// SPDX-License-Identifier: GPL-3.0-or-later
//! Hostile-guest cases at tier V that need only the msb adapter: a root process in the guest
//! tries to reach the network and every vsock address and port, and to make the host attach a
//! file of its choosing as its root disk. Each test boots a real microVM, so they run only under
//! the VM profile (`vm_` prefix).
mod support;

use std::io::Read as _;
use std::path::Path;
use std::time::Duration;

use puddle_compute::{ExecRequest, Runtime, Sandbox, VsockRoute};
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

/// Where the root-disk journal sat in the guest before msb v0.7.12: msb shared the sandbox's
/// runtime folder with the guest, writable, at `/.msb`. msb now exports only a guest-state folder
/// there and keeps the journal next to it on the host side, as `<sandbox>/root-disk.json`.
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

/// As root in the guest, writes a root-disk journal at [`GUEST_JOURNAL`] in msb's shape whose
/// launch base is the sandbox's own disk (`own`, what msb compares with the sandbox's config) and
/// whose only layer is `target`.
#[expect(clippy::expect_used, reason = "a failed exec fails the test")]
async fn forge_journal<S: Sandbox>(sb: &S, own: &Path, target: &Path) {
    let forged = serde_json::json!({
        "schema": "microsandbox.runtime-root-disk/1",
        "launch_base": own,
        "layers": [{ "path": target, "format": "raw" }],
    });
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
        "the guest could not write into msb's guest share: {}",
        out.stderr_text()
    );
}

/// Guest root writes a root-disk journal into msb's guest share at `/.msb` that names another
/// workspace's disk, and then a host file, as its writable root disk (what the old layout's
/// guest-writable journal allowed, upstream advisory GHSA-3f94-vm6x-3jv3). msb keeps its journal
/// out of that share, so the write changes nothing: after each stop the journal on the host is
/// the one msb wrote (or still absent), the next start goes through with the sandbox's own disk
/// (a file written before is still there), and both target files hash the same as before.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn vm_hostile_hg44_a_guest_written_root_disk_journal_changes_nothing_at_the_next_start() {
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
    let sandboxes = settings.home().join("sandboxes");
    let sandbox_dir = sandboxes.join(n.as_str());
    let own = sandbox_dir.join("upper.ext4");
    let other_disk = sandboxes.join(other_name.as_str()).join("upper.ext4");
    assert!(own.is_file(), "no disk at {}", own.display());
    assert!(other_disk.is_file(), "no disk at {}", other_disk.display());
    let host_journal = sandbox_dir.join("root-disk.json");
    let written = sh(&sb, "echo hg44-own-disk > /root/hg44-marker").await;
    assert_eq!(written.status.code, 0, "{}", written.stderr_text());
    let targets = [&other_disk, &host_file];
    let before = targets.map(|t| fingerprint(t));

    // Each round: (journal on the host after the stop, the start's error if any, the marker read
    // after the start).
    let mut rounds = Vec::new();
    for target in targets {
        let journal = std::fs::read(&host_journal).ok();
        forge_journal(&sb, &own, target).await;
        sb.stop().await.unwrap();
        let landed = std::fs::read(&host_journal).ok();
        match rt.start(&n).await {
            Ok(started) => {
                let marker = sh(&started, "cat /root/hg44-marker")
                    .await
                    .stdout_text()
                    .into_owned();
                rounds.push((target.clone(), journal == landed, None, marker));
                sb = started;
            }
            Err(e) => {
                rounds.push((target.clone(), journal == landed, Some(e), String::new()));
                break;
            }
        }
    }
    sb.stop().await.unwrap();
    let after = targets.map(|t| fingerprint(t));
    cleanup(&rt, &[&n, &other_name]).await;
    // The run's home is removed with the run; this only tidies early.
    let _ = std::fs::remove_file(&host_file);
    assert_eq!(rounds.len(), 2, "a start failed: {rounds:?}");
    for (target, unchanged, refused, marker) in rounds {
        assert!(
            unchanged,
            "the guest's write changed msb's journal on the host (target {})",
            target.display()
        );
        assert!(
            refused.is_none(),
            "the start after a guest-written journal naming {} failed: {refused:?}",
            target.display()
        );
        assert_eq!(
            marker.trim(),
            "hg44-own-disk",
            "the sandbox's own disk is not attached"
        );
    }
    assert_eq!(before, after, "a target file changed");
}

/// What a root process can do at the packet level, one `<check> <outcome>` line per step.
/// Phase one: raw IP sockets (ICMP, TCP, UDP, a raw header) send to a public address, and a
/// packet socket bound to each interface but loopback sends a frame; loopback is the control (a
/// second packet socket captures the frame sent on it, and a raw UDP send to 127.0.0.1 goes
/// out). Phase two: root brings the dummy device up with an address and a default route, then
/// tries a TCP connect and a UDP query to public addresses, and a raw TCP segment, which
/// must still reach nothing.
const PACKET_PROBE: &str = r#"
use strict; use warnings; use Socket;
my $payload = "puddle-hg01-frame-" . ("x" x 40);
sub frame { return "\xff\xff\xff\xff\xff\xff" . "\x02\0\0\0\0\x01" . "\x88\xb5" . $payload; }
sub bind_to {
  my ($s, $ifindex) = @_;
  return bind($s, pack("S n i S C C a8", 17, 3, $ifindex, 0, 0, 0, ""));
}
sub ifindex { open(my $f, "<", "/sys/class/net/$_[0]/ifindex") or return; my $i = <$f>; chomp $i; return $i; }
my @devices = grep { $_ ne "lo" } map { s{.*/}{}r } glob("/sys/class/net/*");
print "devices: @devices\n";

# Control: a packet socket on loopback sees the frame another one sends on it.
{
  my $lo = ifindex("lo");
  socket(my $rx, 17, SOCK_RAW, 768) or do { print "control-packet socket failed: $!\n"; goto RAW };
  bind_to($rx, $lo) or do { print "control-packet bind failed: $!\n"; goto RAW };
  socket(my $tx, 17, SOCK_RAW, 768) or do { print "control-packet socket failed: $!\n"; goto RAW };
  bind_to($tx, $lo) or do { print "control-packet bind failed: $!\n"; goto RAW };
  send($tx, frame(), 0) or do { print "control-packet send failed: $!\n"; goto RAW };
  my $seen = 0; my $until = time + 3;
  while (!$seen && time < $until) {
    my $r = ''; vec($r, fileno($rx), 1) = 1;
    next if select($r, undef, undef, 1) <= 0;
    recv($rx, my $buf, 4096, 0) or next;
    $seen = 1 if index($buf, $payload) >= 0;
  }
  print "control-packet ", ($seen ? "captured" : "not-captured"), "\n";
}
RAW:
for my $dev (@devices) {
  my $i = ifindex($dev);
  socket(my $s, 17, SOCK_RAW, 768) or do { print "packet $dev socket failed: $!\n"; next };
  bind_to($s, $i) or do { print "packet $dev bind failed: $!\n"; next };
  send($s, frame(), 0) or do { print "packet $dev send failed: $!\n"; next };
  print "packet $dev sent\n";
}
for my $proto ((1, 6, 17, 255)) {
  for my $dst (("1.1.1.1", "9.9.9.9")) {
    socket(my $s, AF_INET, SOCK_RAW, $proto) or do { print "raw$proto $dst socket failed: $!\n"; next };
    send($s, pack("a24", "puddle-hg01"), 0, pack_sockaddr_in(0, inet_aton($dst)))
      or do { print "raw$proto $dst send failed: $!\n"; next };
    print "raw$proto $dst sent\n";
  }
}
{
  socket(my $s, AF_INET, SOCK_RAW, 17) or do { print "raw-control socket failed: $!\n"; goto PHASE2 };
  send($s, pack("a24", "puddle-hg01"), 0, pack_sockaddr_in(0, inet_aton("127.0.0.1")))
    or do { print "raw-control send failed: $!\n"; goto PHASE2 };
  print "raw-control sent\n";
}
PHASE2:
# Root makes the sink device a network of its own, then tries to leave through it.
if (@devices) {
  my $dev = $devices[0];
  my @steps = ("ip link set $dev up", "ip addr add 192.0.2.2/24 dev $dev",
               "ip route add default via 192.0.2.1 dev $dev");
  for my $cmd (@steps) { system($cmd) == 0 or print "setup failed: $cmd\n"; }
  for my $dst (("140.82.121.4", "1.1.1.1")) {
    socket(my $s, AF_INET, SOCK_STREAM, 0) or do { print "tcp $dst socket failed: $!\n"; next };
    my $ok = eval {
      local $SIG{ALRM} = sub { die "timeout\n" };
      alarm 5;
      connect($s, pack_sockaddr_in(443, inet_aton($dst))) or die "$!\n";
      alarm 0; 1;
    };
    alarm 0;
    print "tcp $dst ", ($ok ? "connected\n" : "failed: $@");
  }
  socket(my $u, AF_INET, SOCK_DGRAM, 0) or print "udp socket failed: $!\n";
  my $query = pack("n6", 0x1234, 0x0100, 1, 0, 0, 0) . "\x06github\x03com\x00" . pack("n2", 1, 1);
  send($u, $query, 0, pack_sockaddr_in(53, inet_aton("1.1.1.1"))) or print "udp send failed: $!\n";
  my $r = ''; vec($r, fileno($u), 1) = 1;
  print "udp 1.1.1.1 ", (select($r, undef, undef, 4) > 0 ? "answered" : "no-answer"), "\n";
}
"#;

/// Past the plain sockets of the first test: as root, raw IP sockets and packet sockets
/// (`AF_PACKET`, the way a tool writes its own frames) reach no network. A raw send to a public
/// address finds no route, and a frame on any device but loopback is refused; and even when
/// root gives the sink device an address and a default route, TCP, UDP and DNS to public
/// addresses reach nothing. The probe is proven by loopback: a second packet socket captures the
/// frame sent on it, and a raw send to 127.0.0.1 goes out.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn vm_hostile_hg01_raw_and_packet_sockets_reach_no_network() {
    let settings = support::settings();
    let rt = support::runtime(&settings).await;
    let s = spec(&settings, "hg01p");
    let n = s.name.clone();
    let sb = rt.create(s).await.expect("create");
    let out = sb
        .exec(
            ExecRequest::new("perl", ["-e", PACKET_PROBE])
                .as_user("root")
                .with_timeout(Duration::from_secs(120)),
        )
        .await
        .expect("packet probe");
    let ip = sh(&sb, "command -v ip").await;
    sb.stop().await.unwrap();
    cleanup(&rt, &[&n]).await;
    let report = out.stdout_text();
    eprintln!("{report}{}", out.stderr_text());
    assert_eq!(out.status.code, 0, "{}", out.stderr_text());
    assert_eq!(
        ip.status.code, 0,
        "the guest has no `ip` to reconfigure with"
    );
    let has = |line: &str| report.lines().any(|l| l == line);
    // The probe works where it may: loopback.
    assert!(
        has("control-packet captured") || report.contains("control-packet socket failed"),
        "the packet probe is not proven by loopback: {report}"
    );
    assert!(has("raw-control sent"), "raw control: {report}");
    // Nothing leaves: no raw send to a public address, no frame on a device but loopback.
    for l in report.lines() {
        let leaked = (l.starts_with("raw") && l.ends_with(" sent"))
            || (l.starts_with("packet ") && l.ends_with(" sent"))
            || l.ends_with("connected")
            || l.ends_with("answered");
        assert!(
            !leaked || l == "raw-control sent",
            "the guest got out: {l}\n{report}"
        );
    }
    assert!(
        !report.contains("setup failed"),
        "root could not set the sink device up, so phase two proved nothing: {report}"
    );
    for dst in ["140.82.121.4", "1.1.1.1"] {
        assert!(
            report
                .lines()
                .any(|l| l.starts_with(&format!("tcp {dst} failed"))),
            "no TCP verdict for {dst}: {report}"
        );
    }
    assert!(has("udp 1.1.1.1 no-answer"), "{report}");
}

/// The vsock addresses the first sweep leaves out: other guests' CIDs, a high one, the largest
/// valid CIDs and `VMADDR_CID_ANY`.
const OTHER_CIDS: [u32; 7] = [4, 5, 6, 100, 0x7fff_ffff, 0xffff_fffe, 0xffff_ffff];

/// The ports tried on every other address: the low ones, the routed one with its neighbours,
/// the usual proxy and web ports and the ephemeral range's start.
const OTHER_CID_PORTS: [&str; 5] = ["1-1024", "3128", "4990-5010", "8080", "49152-49300"];

/// Reads the guest's own CID from `/dev/vsock` (`IOCTL_VM_SOCKETS_GET_LOCAL_CID`).
const LOCAL_CID: &str = r#"open(my $f, "<", "/dev/vsock") or die "open: $!\n"; my $b = pack("L", 0); ioctl($f, 0x7b9, $b) or die "ioctl: $!\n"; print unpack("L", $b), "\n";"#;

/// Past the sweep of the host and CIDs 0, 1 and 3: the routed port accepts on the host and on no
/// other address. Guests' CIDs, the largest valid CIDs, `VMADDR_CID_ANY` and the guest's own
/// CID refuse every port of the sweep, the routed one included, and the same probe is accepted
/// at the host's routed port in the same run (the proof that the probe encodes a CID that
/// connects).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn vm_hostile_hg02_no_other_vsock_address_accepts() {
    let settings = support::settings();
    let rt = support::runtime(&settings).await;
    let root = IpcRoot::new().unwrap();
    let listener = root.listen().unwrap();
    let s = spec(&settings, "hg02o")
        .with_route(VsockRoute::new(ROUTE_PORT, listener.endpoint().path()));
    let n = s.name.clone();
    let lines = collect_lines(listener);
    let sb = rt.create(s).await.expect("create");
    let own = sb
        .exec(ExecRequest::new("perl", ["-e", LOCAL_CID]).as_user("root"))
        .await
        .expect("local cid");
    let own_cid: Option<u32> = own.stdout_text().trim().parse().ok();
    eprintln!("own CID: {own_cid:?} {}", own.stderr_text().trim());
    let control = vsock_sweep(&sb, HOST_CID, "hello-hg02-control", &["5000"]).await;
    let cids: Vec<u32> = OTHER_CIDS.into_iter().chain(own_cid).collect();
    let mut sweeps = Vec::new();
    for cid in &cids {
        let message = format!("hello-hg02-{cid}");
        sweeps.push((
            *cid,
            vsock_sweep(&sb, *cid, &message, &OTHER_CID_PORTS).await,
        ));
    }
    sb.stop().await.unwrap();
    cleanup(&rt, &[&n]).await;
    assert_eq!(
        control.accepted,
        [ROUTE_PORT],
        "the control sweep of the host"
    );
    assert!(sweeps.len() >= OTHER_CIDS.len(), "no sweeps ran");
    for (cid, sweep) in sweeps {
        eprintln!("{}", sweep.summary);
        assert!(
            sweep.accepted.is_empty(),
            "CID {cid} accepted ports {:?}: {}",
            sweep.accepted,
            sweep.summary
        );
    }
    assert_eq!(lines.await.unwrap(), ["hello-hg02-control"]);
}

/// Finds the virtio-mmio register windows (in `/proc/iomem`, else the kernel command line's
/// `virtio_mmio.device=` entries), then does what guest root would do to reach a device's
/// registers past the guest kernel: open `/dev/mem` read-write (or a node it makes for char 1:1
/// when the kernel has none) and map each window read-write. Prints `slot <addr>` per window,
/// `devmem present|absent`, `node <path> open|refused: <why>` and, per window it tried,
/// `<addr> mapped <magic>` or `<addr> refused: <errno>`.
const DEVMEM_PROBE: &str = r#"
use strict; use warnings;
my %seen; my @slots;
if (open(my $io, "<", "/proc/iomem")) {
  while (<$io>) { push @slots, hex($1) if /^\s*([0-9a-f]+)-[0-9a-f]+ : .*virtio/i && !$seen{$1}++ }
}
if (!@slots && open(my $cl, "<", "/proc/cmdline")) {
  my $c = <$cl>;
  push @slots, hex($1) while $c =~ /virtio_mmio\.device=\w+@(0x[0-9a-f]+)/gi;
}
printf "slot 0x%x\n", $_ for @slots;
my $node = "/dev/mem";
my $made = 0;
if (-e $node) { print "devmem present\n" } else {
  print "devmem absent\n";
  $node = "/dev/puddle-probe-mem";
  unlink $node;
  system("mknod", $node, "c", "1", "1") == 0 or do { print "node $node refused: mknod failed\n"; exit 0 };
  $made = 1;
}
my $fd;
if (!open($fd, "+<", $node)) { print "node $node refused: $!\n"; unlink $node if $made; exit 0 }
print "node $node open\n";
for my $slot (@slots) {
  # mmap(NULL, 4096, PROT_READ|PROT_WRITE, MAP_SHARED, fd, slot): x86_64 syscall 9.
  my $addr = syscall(9, 0, 4096, 3, 1, fileno($fd), $slot);
  if ($addr == -1) { printf "0x%x refused: %s\n", $slot, $!; next }
  # A virtio-mmio window starts with its magic value register ("virt").
  printf "0x%x mapped 0x%08x\n", $slot, unpack("L", unpack("P4", pack("Q", $addr)));
  syscall(11, $addr, 4096);
}
unlink $node if $made;
"#;

/// Guest root cannot reach a device's MMIO registers through physical memory: the guest kernel
/// has no `/dev/mem`, and a node root makes for it (char 1:1) cannot be opened, so not one of the
/// virtio-mmio windows maps. With `/dev/mem`, root could map every window read-write and drive a
/// device (the memory balloon among them) past the guest kernel, leaving the device model's own
/// checks as the only line. The probe is proven by finding the windows it tries.
#[cfg_attr(
    windows,
    ignore = "the Windows firmware is built from a kernel configuration that still has /dev/mem"
)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn vm_hostile_hg54_guest_root_cannot_map_device_registers_through_physical_memory() {
    let settings = support::settings();
    let rt = support::runtime(&settings).await;
    let s = spec(&settings, "hg54");
    let n = s.name.clone();
    let sb = rt.create(s).await.expect("create");
    let out = sb
        .exec(
            ExecRequest::new("perl", ["-e", DEVMEM_PROBE])
                .as_user("root")
                .with_timeout(Duration::from_secs(60)),
        )
        .await
        .expect("devmem probe");
    sb.stop().await.unwrap();
    cleanup(&rt, &[&n]).await;
    let report = out.stdout_text();
    eprintln!("{report}{}", out.stderr_text());
    assert_eq!(out.status.code, 0, "{}", out.stderr_text());
    let slots = report.lines().filter(|l| l.starts_with("slot ")).count();
    assert!(slots > 0, "the probe found no virtio-mmio window: {report}");
    let mapped = report.lines().filter(|l| l.contains(" mapped ")).count();
    assert!(
        report.lines().any(|l| l == "devmem absent"),
        "the guest has /dev/mem: {report}"
    );
    assert!(
        report
            .lines()
            .any(|l| l.starts_with("node ") && l.contains(" refused: ")),
        "root opened a physical-memory node: {report}"
    );
    assert_eq!(
        mapped, 0,
        "{mapped} of {slots} device windows mapped: {report}"
    );
}
