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
