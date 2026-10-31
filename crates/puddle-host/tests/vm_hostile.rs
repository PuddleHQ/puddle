// SPDX-License-Identifier: GPL-3.0-or-later
//! Hostile-guest cases on the whole host (K on Linux KVM, W on Windows WHP): a root process in
//! the guest that does not go through Puddle's agent but speaks to the route's vsock port with a
//! yamux client of its own, the way a guest that killed the agent could.
#![expect(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    reason = "VM test: a failed step fails the test; measurements go to stderr"
)]

mod support;
mod vm_support;

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use puddle_host::Host;
use puddle_store::{Actor, Effect, NewRule, Pattern, Scope};
use puddle_types::WorkspaceName;
use serde_json::json;
use support::{Api, ended};
use vm_support::{VmHost, guest, guest_within, start_vm_host};

/// The guest port the host routes to a workspace's agent.
const ROUTE_PORT: u32 = 5000;

/// A yamux client for the guest, written on a raw `AF_VSOCK` socket (perl-base is in every Debian
/// image). It opens one stream per authority argument with `CONNECT <authority>`, the head the
/// agent sends for each proxied connection, and prints `<authority> <status line>`; the first
/// stream, when it is answered `200`, also carries a line to the server and prints what comes
/// back as `echo <line>`.
const YAMUX_CLIENT: &str = r#"
use strict; use warnings; use Socket;
my ($port, @targets) = @ARGV;
socket(my $s, 40, SOCK_STREAM, 0) or die "socket: $!\n";
connect($s, pack("S S L L x4", 40, 0, $port, 2)) or die "connect: $!\n";
$SIG{ALRM} = sub { die "timeout\n" };
alarm 60;
sub send_frame {
  my ($type, $flags, $id, $data) = @_;
  syswrite($s, pack("CCnNN", 0, $type, $flags, $id, length $data) . $data) or die "write: $!\n";
}
sub read_exactly {
  my $want = shift; my $buf = "";
  while (length $buf < $want) {
    my $n = sysread($s, my $chunk, $want - length $buf);
    die "closed\n" unless $n;
    $buf .= $chunk;
  }
  return $buf;
}
# One stream per target, ids 1, 3, 5, ...; a data frame with SYN opens a stream.
my %authority;
my $id = 1;
for my $target (@targets) {
  $authority{$id} = $target;
  send_frame(0, 1, $id, "CONNECT $target HTTP/1.1\r\nHost: $target\r\n\r\n");
  $id += 2;
}
my (%seen, %buffer);
while (keys %seen < @targets) {
  my ($version, $type, $flags, $sid, $len) = unpack("CCnNN", read_exactly(12));
  my $data = $type == 0 && $len ? read_exactly($len) : "";
  next unless exists $authority{$sid};
  $buffer{$sid} .= $data;
  if (!$seen{$sid} && $buffer{$sid} =~ /\r\n\r\n/) {
    $seen{$sid} = 1;
    my ($line) = split /\r\n/, $buffer{$sid};
    print "$authority{$sid} $line\n";
  }
  if (!$seen{$sid} && ($flags & 12)) { $seen{$sid} = 1; print "$authority{$sid} closed-without-an-answer\n"; }
}
if ($buffer{1} =~ m{^HTTP/1.1 200}) {
  send_frame(0, 0, 1, "ping-from-the-guest\n");
  my $tail = "";
  until ($tail =~ /ping-from-the-guest\n/) {
    my ($version, $type, $flags, $sid, $len) = unpack("CCnNN", read_exactly(12));
    my $data = $type == 0 && $len ? read_exactly($len) : "";
    $tail .= $data if $sid == 1;
    die "stream closed before the echo\n" if $sid == 1 && ($flags & 12);
  }
  print "echo ping-from-the-guest\n";
}
"#;

/// Asks the agent's proxy port for each authority the way a tool does and prints
/// `<authority> <status line>`.
const THROUGH_THE_AGENT: &str = r#"
for target in "$@"; do
  exec {fd}<>/dev/tcp/127.0.0.1/3128
  printf 'CONNECT %s HTTP/1.1\r\nHost: %s\r\n\r\n' "$target" "$target" >&"$fd"
  IFS= read -r -u "$fd" status
  echo "$target ${status%$'\r'}"
  exec {fd}>&-
done
"#;

/// A server on the host that echoes every line and counts the connections it accepted.
async fn echo_server(bind: &str) -> (SocketAddr, Arc<AtomicUsize>) {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let listener = tokio::net::TcpListener::bind((bind, 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let accepted = Arc::new(AtomicUsize::new(0));
    let counter = accepted.clone();
    tokio::spawn(async move {
        loop {
            let Ok((conn, _)) = listener.accept().await else {
                return;
            };
            counter.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                let (read, mut write) = conn.into_split();
                let mut lines = BufReader::new(read);
                let mut line = String::new();
                while lines.read_line(&mut line).await.is_ok_and(|n| n > 0) {
                    if write.write_all(line.as_bytes()).await.is_err() {
                        break;
                    }
                    line.clear();
                }
            });
        }
    });
    (addr, accepted)
}

/// Creates workspace `tag` through the API, waits for it to be ready, and allows the host's
/// `127.0.0.1` for it (with the loopback toggle on, since that is a local destination).
async fn workspace_that_reaches_127_0_0_1(
    host: &Host<puddle_compute_msb::MsbRuntime>,
    api: &Api,
    name: &str,
) -> WorkspaceName {
    let sandbox = WorkspaceName::new(name).unwrap();
    host.store()
        .add_rule(&NewRule {
            scope: Scope::Workspace(sandbox.clone()),
            pattern: Pattern::parse("127.0.0.1").unwrap(),
            effect: Effect::Allow,
            expires_at: None,
            created_by: Actor::Cli,
        })
        .unwrap();
    let mut events = api.events().await;
    let name_static: &'static str = Box::leak(name.to_owned().into_boxed_str());
    let reply = api
        .post(
            "/api/workspaces",
            &json!({"name": name, "memory_mib": 512}).to_string(),
        )
        .await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    let end = events
        .until(ended(name_static), Duration::from_secs(600))
        .await;
    assert_eq!(end["step"], "done", "{end}");
    let toggle = api
        .put(
            &format!("/api/settings/workspaces/{name}"),
            &json!({"overrides": {"local_toggles": {"loopback": true}}}).to_string(),
        )
        .await;
    assert_eq!(toggle.status, 200, "{}", toggle.body);
    sandbox
}

/// The connection records of `workspace` to `host`, newest first.
async fn connections(api: &Api, workspace: &str, host: &str) -> Vec<serde_json::Value> {
    let page = api
        .get(&format!(
            "/api/audit?workspace={workspace}&type=connection&host_contains={host}&limit=50"
        ))
        .await
        .json();
    page.get("entries")
        .and_then(serde_json::Value::as_array)
        .unwrap()
        .iter()
        .filter_map(|e| e.get("record").cloned())
        .filter(|r| r.get("host").is_some_and(|h| h == host))
        .collect()
}

/// Stops the workspace through the API and the host after it.
async fn stop(host: Host<puddle_compute_msb::MsbRuntime>, api: &Api, names: &[&str]) {
    for name in names {
        let mut events = api.events().await;
        let name_static: &'static str = Box::leak((*name).to_owned().into_boxed_str());
        let stop = api.post(&format!("/api/workspaces/{name}/stop"), "").await;
        assert_eq!(stop.status, 202, "{}", stop.body);
        let end = events
            .until(ended(name_static), Duration::from_secs(120))
            .await;
        assert_eq!(end["step"], "done", "{end}");
    }
    let down = host.shutdown().await;
    assert!(down.sandboxes.all_stopped(), "{down:?}");
}

/// A guest that kills the agent and speaks to the route's vsock port with a yamux client of its
/// own gets what the agent got: the same answers (a rule's allow carries data to the server, no
/// rule is refused and the server never sees a connection), and every connection is the
/// workspace's own, in its own audit trail and under no other name, whatever the guest writes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vm_hostile_hg03_a_guest_that_killed_the_agent_gets_the_same_policy_and_identity() {
    let VmHost {
        host,
        runtime,
        settings,
        ..
    } = start_vm_host("hg03").await;
    let (allowed, _) = echo_server("127.0.0.1").await;
    // Another loopback address: not allowed by any rule, with a server that must stay untouched.
    let (refused, refused_accepts) = echo_server("127.0.0.2").await;
    let name = format!("{}-hg03", settings.prefix.as_str());
    let api = Api::new(host.url(), host.token());
    let sandbox = workspace_that_reaches_127_0_0_1(&host, &api, &name).await;
    let targets = format!("127.0.0.1:{} 127.0.0.2:{}", allowed.port(), refused.port());

    // The agent's answers, as a tool sees them.
    let through_agent = guest(
        &runtime,
        &sandbox,
        &format!("bash -s {targets} <<'PUDDLE_EOF'\n{THROUGH_THE_AGENT}\nPUDDLE_EOF"),
    )
    .await;
    eprintln!("through the agent:\n{through_agent}");

    // The agent goes away for good: nothing in the guest restarts it.
    guest(&runtime, &sandbox, "kill -9 $(pidof puddle-agent)").await;
    puddle_test_kit::eventually_async("the killed agent gone", Duration::from_secs(30), || async {
        let out = guest(&runtime, &sandbox, "pidof puddle-agent || echo none").await;
        if out.trim() == "none" {
            Ok(())
        } else {
            Err(out)
        }
    })
    .await;

    let direct = guest_within(
        &runtime,
        &sandbox,
        &format!(
            "cat > /tmp/yamux.pl <<'PUDDLE_EOF'\n{YAMUX_CLIENT}\nPUDDLE_EOF\nperl /tmp/yamux.pl {ROUTE_PORT} {targets}"
        ),
        Duration::from_secs(120),
    )
    .await;
    eprintln!("speaking yamux itself:\n{direct}");
    let status_of = |report: &str, authority: &str| {
        report
            .lines()
            .find_map(|l| l.strip_prefix(authority)?.strip_prefix(' '))
            .map_or_else(
                || panic!("no answer for {authority}: {report}"),
                str::to_owned,
            )
    };
    for authority in targets.split(' ') {
        assert_eq!(
            status_of(&direct, authority),
            status_of(&through_agent, authority),
            "{authority}: the route answered the guest's own client differently from the agent"
        );
    }
    assert!(
        status_of(&direct, &format!("127.0.0.1:{}", allowed.port())).starts_with("HTTP/1.1 200"),
        "{direct}"
    );
    assert!(
        !status_of(&direct, &format!("127.0.0.2:{}", refused.port())).contains(" 200"),
        "{direct}"
    );
    assert!(direct.contains("echo ping-from-the-guest"), "{direct}");
    assert_eq!(
        refused_accepts.load(Ordering::SeqCst),
        0,
        "a destination no rule allows was connected to"
    );

    // Identity: both hosts are in this workspace's trail, the refused one not as an allow, and
    // no other workspace has a record of them.
    puddle_test_kit::eventually_async(
        "this workspace's records for both destinations",
        Duration::from_secs(30),
        || async {
            let allowed_rows = connections(&api, &name, "127.0.0.1").await;
            let refused_rows = connections(&api, &name, "127.0.0.2").await;
            if allowed_rows.iter().any(|r| r["decision"] == "allow")
                && !refused_rows.is_empty()
                && refused_rows.iter().all(|r| r["decision"] != "allow")
            {
                Ok(())
            } else {
                Err(format!("{allowed_rows:#?}\n{refused_rows:#?}"))
            }
        },
    )
    .await;
    // Every record of these destinations, whoever's, is this workspace's: the guest's own client
    // cannot put a connection in another workspace's trail.
    let everyone = api
        .get("/api/audit?type=connection&host_contains=127.0.0.&limit=500")
        .await
        .json();
    let everyone = everyone["entries"].as_array().unwrap().len();
    let mine = connections(&api, &name, "127.0.0.1").await.len()
        + connections(&api, &name, "127.0.0.2").await.len();
    eprintln!("records of the two destinations: {everyone}, this workspace's: {mine}");
    assert!(mine > 0, "no record of this workspace");
    assert_eq!(
        everyone, mine,
        "a record of these destinations is not this workspace's"
    );

    stop(host, &api, &[&name]).await;
}

/// Streams the hostile guest asks for at once: above the per-workspace cap, so the host must
/// refuse some.
const FLOOD_STREAMS: usize = 4096 + 512;

/// Opens `FLOOD` streams through the yamux client's session, all `CONNECT <authority>`, waits for
/// every status line, writes the counts to `/tmp/hg11.out`, and holds the streams open until
/// `/tmp/hg11.release` exists.
const FLOOD_SCRIPT: &str = r#"
use strict; use warnings; use Socket;
my ($port, $count, $target) = @ARGV;
socket(my $s, 40, SOCK_STREAM, 0) or die "socket: $!\n";
connect($s, pack("S S L L x4", 40, 0, $port, 2)) or die "connect: $!\n";
$SIG{ALRM} = sub { die "timeout\n" };
alarm 900;
my $head = "CONNECT $target HTTP/1.1\r\nHost: $target\r\n\r\n";
my $out = "";
for my $i (0 .. $count - 1) {
  $out .= pack("CCnNN", 0, 0, 1, 2 * $i + 1, length $head) . $head;
}
# Write in a child, so that reading never waits on a full buffer.
my $pid = fork();
die "fork: $!" unless defined $pid;
if (!$pid) {
  open(my $log, ">", "/tmp/hg11.child") or die;
  my $off = 0;
  select((select($log), $| = 1)[0]);
  print $log "start ", length $out, "\n";
  while ($off < length $out) {
    my $n = syswrite($s, $out, 4096, $off);
    print $log "wrote $n at $off\n" if $off < 20000;
    unless ($n) { print $log "write failed at $off: $!\n"; close $log; exit 1; }
    $off += $n;
  }
  print $log "wrote $off\n"; close $log; exit 0;
}
sub read_exactly {
  my $want = shift; my $buf = "";
  while (length $buf < $want) {
    my $n = sysread($s, my $chunk, $want - length $buf);
    die "closed\n" unless $n;
    $buf .= $chunk;
  }
  return $buf;
}
# The host drops a session that stays silent; a real agent pings, so does this client.
sub ping { syswrite($s, pack("CCnNN", 0, 2, 1, 0, 7)); }
sub reply_to_ping { my ($flags, $len) = @_; syswrite($s, pack("CCnNN", 0, 2, 2, 0, $len)) if $flags & 1; }
my (%buffer, %seen, %count);
my $error = "";
eval {
  while (keys %seen < $count) {
    my ($version, $type, $flags, $sid, $len) = unpack("CCnNN", read_exactly(12));
    if ($type == 2) { reply_to_ping($flags, $len); next; }
    my $data = $type == 0 && $len ? read_exactly($len) : "";
    next unless $sid % 2 == 1;
    $buffer{$sid} .= $data;
    if (!$seen{$sid} && $buffer{$sid} =~ /\r\n\r\n/) {
      $seen{$sid} = 1;
      my ($line) = split /\r\n/, $buffer{$sid};
      $count{(split / /, $line)[1]}++;
      if (keys(%seen) % 500 == 0) { open(my $pg, ">", "/tmp/hg11.progress"); print $pg keys(%seen), " at ", time - $^T, "s\n"; close $pg; }
      delete $buffer{$sid};
    }
  }
  1;
} or $error = " error=$@";
chomp $error;
$error .= " child_alive=" . (kill(0, $pid) ? 1 : 0) . " elapsed=" . (time - $^T);
open(my $o, ">", "/tmp/hg11.out.tmp") or die;
print $o join(" ", map { "$_=$count{$_}" } sort keys %count), " seen=", scalar(keys %seen), "$error\n";
close $o;
rename "/tmp/hg11.out.tmp", "/tmp/hg11.out";
until (-e "/tmp/hg11.release") {
  ping();
  my $r = ''; vec($r, fileno($s), 1) = 1;
  while (select($r, undef, undef, 3) > 0) {
    my ($version, $type, $flags, $sid, $len) = unpack("CCnNN", read_exactly(12));
    read_exactly($len) if $type == 0 && $len;
    reply_to_ping($flags, $len) if $type == 2;
    $r = ''; vec($r, fileno($s), 1) = 1;
    last if -e "/tmp/hg11.release";
  }
}
"#;

/// A flood from one workspace's guest, speaking yamux itself with more streams than the cap:
/// the host holds the workspace at its cap and refuses the rest with the `503` that names it,
/// and while that workspace is held at the cap another workspace's connection finishes inside
/// the transparency bar. When the flood ends, the first workspace's slots are free again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vm_hostile_hg11_a_flooding_workspace_is_held_at_its_cap_and_another_is_not_slowed() {
    let VmHost {
        host,
        runtime,
        settings,
        ..
    } = start_vm_host("hg11").await;
    let cap = puddle_proxy::ProxyConfig::default().max_streams_per_workspace;
    assert!(cap < FLOOD_STREAMS, "the flood must be over the cap");
    let limit = puddle_fd_limit::raise_open_file_limit(puddle_fd_limit::Reach::Soft);
    assert!(
        limit.allows(4 * FLOOD_STREAMS as u64),
        "the host process cannot hold the flood: {limit}"
    );
    let (server, _) = echo_server("127.0.0.1").await;
    let api = Api::new(host.url(), host.token());
    let flood = format!("{}-hg11a", settings.prefix.as_str());
    let bystander = format!("{}-hg11b", settings.prefix.as_str());
    let flooder = workspace_that_reaches_127_0_0_1(&host, &api, &flood).await;
    let other = workspace_that_reaches_127_0_0_1(&host, &api, &bystander).await;
    let target = format!("127.0.0.1:{}", server.port());

    // The flood runs in the background of the guest and holds its streams.
    guest(
        &runtime,
        &flooder,
        &format!(
            "cat > /tmp/flood.pl <<'PUDDLE_EOF'\n{FLOOD_SCRIPT}\nPUDDLE_EOF\n\
             cat > /tmp/yamux.pl <<'PUDDLE_EOF'\n{YAMUX_CLIENT}\nPUDDLE_EOF\n\
             kill -9 $(pidof puddle-agent)\n\
             nohup perl /tmp/flood.pl {ROUTE_PORT} {FLOOD_STREAMS} {target} >/tmp/hg11.log 2>&1 &"
        ),
    )
    .await;
    let counts = puddle_test_kit::eventually_async(
        "the flood's counts",
        Duration::from_secs(95),
        || async {
            let out = guest(
                &runtime,
                &flooder,
                "cat /tmp/hg11.out 2>/dev/null || { echo pending; tail -3 /tmp/hg11.log; cat /tmp/hg11.progress; }",
            )
            .await;
            if out.starts_with("pending") {
                Err(out)
            } else {
                Ok(out)
            }
        },
    )
    .await;
    eprintln!("flood counts: {counts}");
    assert_eq!(
        counts.trim(),
        format!("200={cap} 503={} seen={FLOOD_STREAMS}", FLOOD_STREAMS - cap),
        "the host did not hold the workspace at its cap"
    );

    // While the flooder sits at the cap, the other workspace finishes a connection and an echo.
    let started = Instant::now();
    let report = guest(
        &runtime,
        &other,
        &format!("bash -s {target} <<'PUDDLE_EOF'\n{THROUGH_THE_AGENT}\nPUDDLE_EOF"),
    )
    .await;
    let took = started.elapsed();
    eprintln!("the other workspace while the flood is held: {report}took {took:?}");
    assert!(
        report.contains(&format!("{target} HTTP/1.1 200")),
        "{report}"
    );
    assert!(took < Duration::from_secs(5), "the bystander took {took:?}");

    // The flood ends: its slots are free for the workspace again (the agent is gone, so the
    // guest's own client asks).
    guest(&runtime, &flooder, "touch /tmp/hg11.release").await;
    let again = puddle_test_kit::eventually_async(
        "the flooder's slots free after the flood",
        Duration::from_secs(120),
        || async {
            let out = guest(
                &runtime,
                &flooder,
                &format!("perl /tmp/yamux.pl {ROUTE_PORT} {target} 2>&1 || true"),
            )
            .await;
            if out.contains(&format!("{target} HTTP/1.1 200")) {
                Ok(out)
            } else {
                Err(out)
            }
        },
    )
    .await;
    eprintln!("after the flood: {again}");
    stop(host, &api, &[&flood, &bystander]).await;
}
