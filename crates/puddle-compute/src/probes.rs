// SPDX-License-Identifier: GPL-3.0-or-later
//! The guest-side programs of the hostile-guest contract cases, and how to tell them apart.
//!
//! Each script starts with a `# puddle-probe: <name>` line. A real runtime runs it in the guest
//! as written. [`crate::fake::FakeRuntime`] has no guest to run it in: it finds the marker with
//! [`Probe::of`] and answers as a guest with no network and only its routed vsock ports would.

use crate::ExecRequest;

/// The first characters of the line that names a probe.
const MARKER: &str = "# puddle-probe: ";

/// A guest program of the contract suite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Probe {
    /// [`NO_DIRECT_NETWORK`]: a shell script that exits 0 when nothing reaches out.
    NoDirectNetwork,
    /// [`ICMP_PROBE`].
    Icmp,
    /// [`PACKET_PROBE`].
    Packet,
    /// [`VSOCK_SWEEP`].
    VsockSweep,
    /// [`LOCAL_CID`].
    LocalCid,
}

impl Probe {
    fn named(name: &str) -> Option<Self> {
        Some(match name {
            "no-direct-network" => Self::NoDirectNetwork,
            "icmp" => Self::Icmp,
            "packet" => Self::Packet,
            "vsock-sweep" => Self::VsockSweep,
            "local-cid" => Self::LocalCid,
            _ => return None,
        })
    }

    /// The probe `request` runs, with the arguments after its script, if one of its arguments
    /// is a probe script.
    #[must_use]
    pub fn of(request: &ExecRequest) -> Option<(Self, Vec<&str>)> {
        let at = request.args.iter().position(|a| a.contains(MARKER))?;
        let script = request.args.get(at)?;
        let name = script.split_once(MARKER)?.1.lines().next()?.trim();
        let rest = request
            .args
            .get(at + 1..)?
            .iter()
            .map(String::as_str)
            .collect();
        Some((Self::named(name)?, rest))
    }
}

/// As root: no interface but loopback is up, and DNS, IPv4 TCP, UDP and IPv6 reach nothing, while
/// loopback still works. Exits 0 when all of that holds; prints what failed.
pub const NO_DIRECT_NETWORK: &str = r#"
# puddle-probe: no-direct-network
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

/// Sends one ICMP echo request from a raw socket to each address given and waits 3 s for the
/// echo reply. Prints `<address> reply`, `<address> no-reply` or `<address> <step> failed: <errno>`.
pub const ICMP_PROBE: &str = r#"
# puddle-probe: icmp
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

/// What a root process can do at the packet level, one `<check> <outcome>` line per step.
/// Phase one: raw IP sockets (ICMP, TCP, UDP, a raw header) send to a public address, and a
/// packet socket bound to each interface but loopback sends a frame; loopback is the control (a
/// second packet socket captures the frame sent on it, and a raw UDP send to 127.0.0.1 goes
/// out). Phase two: root brings the dummy device up with an address and a default route, then
/// tries a TCP connect and a UDP query to public addresses, and a raw TCP segment, which
/// must still reach nothing.
pub const PACKET_PROBE: &str = r#"
# puddle-probe: packet
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

/// Connects to host or neighbour CIDs over `AF_VSOCK` in batches of non-blocking connects and
/// prints `accepted <port>` for each port that took the connection, then sends it a line.
/// perl-base is in every Debian image and can open an `AF_VSOCK` socket by number. Arguments:
/// the line to send, the CID, then ports or `lo-hi` ranges. A summary of how the other ports
/// ended goes to stderr.
pub const VSOCK_SWEEP: &str = r#"
# puddle-probe: vsock-sweep
use strict; use warnings; use Socket; use Fcntl;
# usage: sweep.pl MESSAGE CID PORT|LO-HI ...
# Non-blocking connects in batches; prints "accepted PORT" per accepted port and a summary line.
my ($msg, $cid, @specs) = @ARGV;
my @ports;
for my $spec (@specs) {
  my ($lo, $hi) = $spec =~ /^(\d+)-(\d+)$/ ? ($1, $2) : ($spec, $spec);
  push @ports, $lo .. $hi;
}
my $BATCH = 4096;
my (%outcome, @accepted);
while (my @batch = splice(@ports, 0, $BATCH)) {
  my (%pending, @open);
  my $wvec = '';
  for my $port (@batch) {
    socket(my $s, 40, SOCK_STREAM, 0) or die "socket: $!";
    fcntl($s, F_SETFL, fcntl($s, F_GETFL, 0) | O_NONBLOCK) or die "fcntl: $!";
    if (connect($s, pack("S S L L x4", 40, 0, $port, $cid))) {
      push @open, [$s, $port];
    } elsif ($!{EINPROGRESS}) {
      vec($wvec, fileno($s), 1) = 1;
      $pending{fileno($s)} = [$s, $port];
    } else {
      $outcome{"$!"}++;
      close $s;
    }
  }
  my $deadline = time + 15;
  while (%pending && time < $deadline) {
    my $w = $wvec;
    next if select(undef, $w, undef, 1) <= 0;
    for my $fd (keys %pending) {
      next unless vec($w, $fd, 1);
      my ($s, $port) = @{ delete $pending{$fd} };
      vec($wvec, $fd, 1) = 0;
      my $err = unpack("i", getsockopt($s, SOL_SOCKET, SO_ERROR));
      if ($err == 0) { push @open, [$s, $port]; }
      else { local $! = $err; $outcome{"$!"}++; close $s; }
    }
  }
  for my $p (values %pending) { $outcome{"no answer"}++; close $p->[0]; }
  for my $o (@open) {
    my ($s, $port) = @$o;
    $outcome{accepted}++;
    push @accepted, $port;
    select((select($s), $| = 1)[0]);
    # Blocking again for the send.
    fcntl($s, F_SETFL, fcntl($s, F_GETFL, 0) & ~O_NONBLOCK);
    print {$s} "$msg\n";
    print "accepted $port\n";
    # Linger before closing: msb on Windows can lose the last bytes of a stream the guest closes
    # at once (tail loss), which is not what this probe tests.
    select(undef, undef, undef, 0.3);
    close $s;
  }
}
print STDERR "cid $cid: ", join(", ", map { "$_ x$outcome{$_}" } sort keys %outcome), "\n";
"#;

/// Reads the guest's own vsock CID from `/dev/vsock` (`IOCTL_VM_SOCKETS_GET_LOCAL_CID`).
pub const LOCAL_CID: &str = r#"# puddle-probe: local-cid
open(my $f, "<", "/dev/vsock") or die "open: $!\n"; my $b = pack("L", 0); ioctl($f, 0x7b9, $b) or die "ioctl: $!\n"; print unpack("L", $b), "\n";"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_probe_is_found_by_its_marker_and_hands_over_the_arguments_after_its_script() {
        let request = ExecRequest::new("perl", ["-e", ICMP_PROBE, "127.0.0.1", "1.1.1.1"]);
        assert_eq!(
            Probe::of(&request),
            Some((Probe::Icmp, vec!["127.0.0.1", "1.1.1.1"]))
        );
        assert_eq!(
            Probe::of(&ExecRequest::sh(NO_DIRECT_NETWORK)).map(|p| p.0),
            Some(Probe::NoDirectNetwork)
        );
        assert_eq!(
            Probe::of(&ExecRequest::new("perl", ["-e", PACKET_PROBE])).map(|p| p.0),
            Some(Probe::Packet)
        );
        assert_eq!(
            Probe::of(&ExecRequest::new("perl", ["-e", LOCAL_CID])).map(|p| p.0),
            Some(Probe::LocalCid)
        );
        assert_eq!(
            Probe::of(&ExecRequest::new("perl", ["-e", VSOCK_SWEEP, "m", "2"])).map(|p| p.0),
            Some(Probe::VsockSweep)
        );
    }

    #[test]
    fn other_requests_and_unknown_marker_names_are_no_probe() {
        assert_eq!(Probe::of(&ExecRequest::sh("exit 0")), None);
        let unknown = ExecRequest::sh("# puddle-probe: made-up\nexit 0");
        assert_eq!(Probe::of(&unknown), None);
    }
}
