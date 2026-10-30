// SPDX-License-Identifier: GPL-3.0-or-later
//! I tests for transparent capture, with the real kernel: each test runs in its own network
//! namespace (`unshare -Urnm`, the way `real_bridge.rs` does), with a stand-in for the VM's network
//! device (`eth0`, a default route), the agent, and a fake host behind the agent's Unix socket
//! that records what reaches it. The clients are real tools with no proxy variable at all: `curl`,
//! `dig`, `bash`'s `/dev/tcp`, Python's UDP sockets.
//!
//! A test re-runs itself inside the namespace (`enter`) and fails, not skips, when no
//! namespace can be made. `nft` is used to look at the kernel's table and to be the guest that
//! tampers with it.
#![cfg(target_os = "linux")]
#![expect(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "helpers outside #[test] fns, and the shared harness module, run only in tests"
)]

use crate::common;

use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use puddle_agent::config::{CaptureConfig, DnsConfig, Target};
use puddle_agent::{Agent, Config};
use puddle_agent_proto::host::{GuestStream, HostConfig, StreamHandler, UdpRefusal, serve_session};
use puddle_agent_proto::resolve::{RecordType, ResolveAnswer, ResolveQuery, StandInReason};
use puddle_test_kit::eventually_async;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, UnixListener};
use tokio::process::Command;

const CHILD: &str = "PUDDLE_CAPTURE_NETNS";

/// `sudo` drops the environment, and with it the coverage run's profile path: pass it on by hand so
/// the code that runs in the namespace is counted.
fn profile_env() -> Option<String> {
    std::env::var("LLVM_PROFILE_FILE")
        .ok()
        .map(|path| format!("LLVM_PROFILE_FILE={path}"))
}

/// Runs the calling test again inside a fresh network namespace and returns `false` (the caller
/// returns); inside the namespace returns `true` and the caller runs its body.
async fn enter(test: &str) -> bool {
    if std::env::var_os(CHILD).is_some() {
        return true;
    }
    let exe = std::env::current_exe().unwrap();
    let mut last = String::new();
    for runner in [
        vec!["unshare", "-Urnm"],
        vec!["sudo", "-n", "unshare", "-nm"],
    ] {
        let output = Command::new(runner[0])
            .args(&runner[1..])
            .arg("env")
            .arg(format!("{CHILD}=1"))
            .args(profile_env())
            .arg(&exe)
            .args([
                "--exact",
                &format!("capture::{test}"),
                "--nocapture",
                "--test-threads=1",
            ])
            .kill_on_drop(true)
            .output()
            .await
            .unwrap();
        let report = format!(
            "--- stdout\n{}\n--- stderr\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        if output.status.success() {
            return false;
        }
        // A namespace that could not be made says so before any test output.
        if report.contains("unshare:") || report.contains("sudo:") {
            last = report;
            continue;
        }
        panic!("{test} failed inside its namespace\n{report}");
    }
    panic!(
        "no way to create a network namespace: need `unshare -Urnm` or passwordless `sudo unshare -nm`\n{last}"
    );
}

/// What the fake host saw.
#[derive(Debug, Default)]
struct Seen {
    connects: Mutex<Vec<Connect>>,
    udp: Mutex<Vec<UdpRefusal>>,
    tampered: Mutex<Vec<bool>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Connect {
    authority: String,
    /// The request carried `x-puddle-protocol: ssh`.
    ssh_header: bool,
    /// The client's first bytes, right behind the head.
    behind: Vec<u8>,
}

#[derive(Clone)]
struct Fake(Arc<Seen>);

impl StreamHandler for Fake {
    async fn handle(&self, stream: GuestStream) {
        let mut reader = BufReader::new(stream);
        let mut head = String::new();
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                return;
            }
            let end = line == "\r\n";
            head.push_str(&line);
            if end {
                break;
            }
        }
        let authority = head
            .lines()
            .next()
            .and_then(|l| l.split(' ').nth(1))
            .unwrap_or_default()
            .to_owned();
        let ssh_header = head.to_ascii_lowercase().contains("x-puddle-protocol: ssh");
        let behind = reader.buffer().to_vec();
        self.0.connects.lock().unwrap().push(Connect {
            authority: authority.clone(),
            ssh_header,
            behind: behind.clone(),
        });
        let mut guest = reader.into_inner();
        if ssh_header || behind.starts_with(b"SSH-") {
            refuse(
                &mut guest,
                "ssh_unsupported",
                "puddle: SSH is not supported yet, use HTTPS\n",
            )
            .await;
        } else if authority.contains("deny.test") {
            refuse(&mut guest, "no_rule", "puddle: blocked\n").await;
        } else {
            guest
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .await
                .unwrap();
            serve(&mut guest, &authority, behind).await;
        }
    }

    fn resolve(&self, query: ResolveQuery) -> impl Future<Output = ResolveAnswer> + Send {
        let answer =
            if query.rtype == RecordType::A && query.name.to_ascii_lowercase().ends_with(".test") {
                ResolveAnswer::StandIn {
                    why: StandInReason::Resolves,
                    ttl: 60,
                }
            } else {
                ResolveAnswer::NoSuchName { ttl: 20 }
            };
        async move { answer }
    }

    async fn udp_refused(&self, refusal: UdpRefusal) {
        self.0.udp.lock().unwrap().push(refusal);
    }

    async fn capture_tampered(&self, table_missing: bool) {
        self.0.tampered.lock().unwrap().push(table_missing);
    }
}

async fn refuse(guest: &mut GuestStream, code: &str, body: &str) {
    let text = format!(
        "HTTP/1.1 403 Forbidden\r\ncontent-length: {}\r\nx-puddle-blocked: {code}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    guest.write_all(text.as_bytes()).await.unwrap();
    guest.shutdown().await.unwrap();
}

/// The server behind the tunnel: answers a GET with a page naming the authority and echoes
/// anything else.
async fn serve(guest: &mut GuestStream, authority: &str, mut data: Vec<u8>) {
    let mut chunk = [0u8; 4096];
    loop {
        if data.is_empty() {
            match guest.read(&mut chunk).await {
                Ok(n) if n > 0 => data.extend_from_slice(&chunk[..n]),
                _ => return,
            }
        }
        if data.starts_with(b"GET ") {
            let body = format!("hello from {authority}");
            let page = format!(
                "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = guest.write_all(page.as_bytes()).await;
            let _ = guest.shutdown().await;
            return;
        }
        if guest.write_all(&data).await.is_err() {
            return;
        }
        data.clear();
    }
}

fn start_host(socket: &Path, seen: &Arc<Seen>) {
    let listener = UnixListener::bind(socket).unwrap();
    let handler = Fake(Arc::clone(seen));
    tokio::spawn(async move {
        loop {
            let (conn, _) = listener.accept().await.unwrap();
            let handler = handler.clone();
            tokio::spawn(async move {
                let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
                let _ = serve_session(
                    conn,
                    common::workspace(),
                    Arc::new(common::ChanSink(tx)),
                    Arc::new(handler),
                    HostConfig::default(),
                )
                .await;
            });
        }
    });
}

/// Runs a command, returning its status and what it printed (stdout then stderr), within `limit`.
async fn run(program: &str, args: &[&str], limit: Duration) -> (bool, String) {
    let output = tokio::time::timeout(
        limit,
        Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .unwrap_or_else(|_| panic!("{program} {args:?} did not finish in {limit:?}"))
    .unwrap_or_else(|e| panic!("{program}: {e}"));
    (
        output.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    )
}

async fn ok(program: &str, args: &[&str]) -> String {
    let (success, text) = run(program, args, Duration::from_secs(20)).await;
    assert!(success, "{program} {args:?} failed: {text}");
    text
}

async fn bash(script: &str) -> (bool, String) {
    run("bash", &["-c", script], Duration::from_secs(20)).await
}

/// The network the VM runtime gives a guest, as a dummy device: an address, a default route.
async fn guest_network() {
    for args in [
        &["link", "set", "lo", "up"][..],
        &["link", "add", "eth0", "type", "dummy"],
        &["addr", "add", "172.16.0.6/30", "dev", "eth0"],
        &["link", "set", "eth0", "up"],
        &["route", "add", "default", "via", "172.16.0.5"],
        &["-6", "addr", "add", "fd42::6/64", "dev", "eth0", "nodad"],
        &["-6", "route", "add", "default", "via", "fd42::5"],
    ] {
        ok("ip", args).await;
    }
}

/// A Docker-like bridge that owns `172.17.0.1`.
async fn docker_bridge() {
    for args in [
        &["link", "add", "docker0", "type", "bridge"][..],
        &["addr", "add", "172.17.0.1/16", "dev", "docker0"],
        &["link", "set", "docker0", "up"],
    ] {
        ok("ip", args).await;
    }
}

fn config(socket: &Path, table: &Path) -> Config {
    Config {
        listen: SocketAddr::from(([127, 0, 0, 1], 0)),
        bridge: None,
        target: Target::Unix(socket.to_path_buf()),
        oom: None,
        dns: Some(DnsConfig {
            listen: "198.18.0.1:53".parse().unwrap(),
            table: Some(table.to_path_buf()),
        }),
        capture: Some(CaptureConfig {
            poll: Duration::from_millis(100),
            ..CaptureConfig::default()
        }),
        ..Config::default()
    }
}

/// How long a capture test waits for the host or the table to settle.
const WAIT: Duration = Duration::from_secs(15);

/// Capture is ready when the table is in and the stub answers.
async fn ready() {
    eventually_async("the capture table and the stub", WAIT, || async {
        let (_, text) = run(
            "dig",
            &["+short", "+time=1", "+tries=1", "@198.18.0.1", "ready.test"],
            Duration::from_secs(5),
        )
        .await;
        text.starts_with("198.18.") && puddle_agent::capture::nft::read().is_ok_and(|t| t.is_some())
    })
    .await;
}

/// Points the resolver at the stub for tools that read `/etc/resolv.conf`.
async fn stub_resolver(dir: &Path) {
    let file = dir.join("resolv.conf");
    std::fs::write(
        &file,
        "nameserver 198.18.0.1\noptions timeout:2 attempts:1\n",
    )
    .unwrap();
    ok(
        "mount",
        &["--bind", file.to_str().unwrap(), "/etc/resolv.conf"],
    )
    .await;
}

struct World {
    agent: Agent,
    seen: Arc<Seen>,
    dir: common::TempDir,
}

async fn world(tag: &str) -> World {
    guest_network().await;
    let dir = common::TempDir::new(tag);
    let seen = Arc::new(Seen::default());
    let socket = dir.0.join("route.sock");
    start_host(&socket, &seen);
    let agent = Agent::start(config(&socket, &dir.0.join("table")))
        .await
        .unwrap();
    ready().await;
    World { agent, seen, dir }
}

impl World {
    fn connects(&self) -> Vec<Connect> {
        self.seen.connects.lock().unwrap().clone()
    }

    fn authorities(&self) -> Vec<String> {
        self.connects().into_iter().map(|c| c.authority).collect()
    }
}

#[tokio::test]
#[expect(clippy::too_many_lines, reason = "one story, read top to bottom")]
async fn tools_with_no_proxy_settings_reach_the_proxy_by_name_and_by_address() {
    if !enter("tools_with_no_proxy_settings_reach_the_proxy_by_name_and_by_address").await {
        // skip-ok: the outer run re-ran this test in a namespace and asserted that it passed
        return;
    }
    docker_bridge().await;
    let w = world("redirect").await;
    stub_resolver(&w.dir.0).await;

    // DNS to any resolver is the stub's: a stand-in for a name the host allows.
    let answer = ok(
        "dig",
        &["+short", "+time=1", "+tries=1", "@8.8.8.8", "web.test"],
    )
    .await;
    assert!(answer.starts_with("198.18."), "{answer}");

    // A tool that resolves and connects, with an empty environment.
    let (success, page) = run(
        "env",
        &[
            "-i",
            "curl",
            "-sS",
            "--max-time",
            "5",
            "http://web.test:8080/",
        ],
        Duration::from_secs(10),
    )
    .await;
    assert!(success, "{page}");
    assert_eq!(page, "hello from web.test:8080");
    assert!(
        w.authorities().contains(&"web.test:8080".to_owned()),
        "{:?}",
        w.authorities()
    );

    // A raw TCP client that was handed a real address: asked for by address.
    let (success, line) =
        bash(r#"exec 3<>/dev/tcp/192.0.2.7/5432; printf 'ping\n' >&3; read -t 3 l <&3; echo "$l""#)
            .await;
    assert!(success && line.trim() == "ping", "{line}");
    assert!(w.authorities().contains(&"192.0.2.7:5432".to_owned()));

    // A client that waits for the server first still gets through, and sent nothing with the head.
    let (success, line) = bash(
        r#"exec 3<>/dev/tcp/db.test/5433 2>/dev/null || exec 3<>/dev/tcp/$(dig +short @198.18.0.1 db.test)/5433;
           sleep 0.3; printf 'late\n' >&3; read -t 3 l <&3; echo "$l""#,
    )
    .await;
    assert!(success && line.trim() == "late", "{line}");
    let late = w
        .connects()
        .into_iter()
        .find(|c| c.authority == "db.test:5433")
        .unwrap();
    assert!(late.behind.is_empty() && !late.ssh_header, "{late:?}");

    // What is inside the guest is not captured: loopback and the bridge go straight there.
    let before = w.authorities().len();
    for bind in ["127.0.0.1", "172.17.0.1"] {
        let server = TcpListener::bind((bind, 0)).await.unwrap();
        let port = server.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut c, _) = server.accept().await.unwrap();
            c.write_all(b"direct\n").await.unwrap();
        });
        let (success, line) = bash(&format!(
            "exec 3<>/dev/tcp/{bind}/{port}; read -t 3 l <&3; echo \"$l\""
        ))
        .await;
        assert!(success && line.trim() == "direct", "{bind}: {line}");
    }
    assert_eq!(
        w.authorities().len(),
        before,
        "local connections reached the proxy"
    );

    // Dialling the capture port yourself has no original destination: closed, nothing sent up.
    let port = w.agent.capture_addr().unwrap().port();
    let (_, text) = bash(&format!(
        "exec 3<>/dev/tcp/127.0.0.1/{port}; read -t 2 l <&3; echo \"got:[$l] rc=$?\""
    ))
    .await;
    assert!(!text.contains("got:[hello"), "{text}");
    assert_eq!(w.authorities().len(), before);

    // A stand-in nobody handed out has no name to ask for: reset, nothing asked of the host.
    let started = Instant::now();
    let (success, _) = bash("exec 3<>/dev/tcp/198.18.200.1/80; read -t 3 l <&3").await;
    assert!(
        !success || started.elapsed() < Duration::from_secs(2),
        "an unknown stand-in hung"
    );
    assert_eq!(w.authorities().len(), before);

    // A refusal is a reset, soon, never a hang.
    let started = Instant::now();
    let (success, text) = run(
        "env",
        &[
            "-i",
            "curl",
            "-sS",
            "--max-time",
            "5",
            "http://x.deny.test:8080/",
        ],
        Duration::from_secs(10),
    )
    .await;
    assert!(
        !success && started.elapsed() < Duration::from_secs(3),
        "{text}"
    );
    assert!(w.authorities().contains(&"x.deny.test:8080".to_owned()));

    // A raw SSH client that never read ssh_config: recognised from its first bytes, refused with
    // the proxy's words ahead of any banner.
    let (_, text) = bash(
        "exec 3<>/dev/tcp/192.0.2.22/22; printf 'SSH-2.0-libssh2_1.11\\r\\n' >&3; timeout 3 cat <&3",
    )
    .await;
    assert!(text.contains("SSH is not supported yet"), "{text}");
    let ssh = w
        .connects()
        .into_iter()
        .find(|c| c.authority == "192.0.2.22:22")
        .unwrap();
    assert!(
        ssh.ssh_header && ssh.behind.starts_with(b"SSH-2.0-libssh2"),
        "{ssh:?}"
    );
}

#[tokio::test]
async fn udp_that_is_not_dns_and_ipv6_fail_at_once_and_the_host_is_told_about_the_udp() {
    if !enter("udp_that_is_not_dns_and_ipv6_fail_at_once_and_the_host_is_told_about_the_udp").await
    {
        return;
    }
    let w = world("udp").await;
    let probe = r#"
import socket, sys, time
host, port = sys.argv[1], int(sys.argv[2])
family = socket.AF_INET6 if ":" in host else socket.AF_INET
s = socket.socket(family, socket.SOCK_DGRAM)
s.settimeout(3)
s.connect((host, port))
start = time.monotonic()
try:
    s.send(b"x")
    s.recv(16)
    print("answered")
except ConnectionRefusedError:
    print("refused %.1f" % ((time.monotonic() - start) * 1000))
except OSError as e:
    print("oserror %s %.1f" % (e.errno, (time.monotonic() - start) * 1000))
"#;
    for (host, port) in [
        ("192.0.2.9", "9999"),
        ("198.18.77.7", "4444"),
        ("2001:db8::9", "443"),
    ] {
        let text = ok("python3", &["-c", probe, host, port]).await;
        let (kind, ms) = text.trim().split_once(' ').unwrap_or((text.trim(), "0"));
        let ms: f64 = ms.parse().unwrap_or(0.0);
        assert!(kind == "refused" || kind == "oserror", "{host}: {text}");
        assert!(ms < 50.0, "{host} took {ms} ms: {text}");
    }
    // IPv6 TCP: no capture for it, fails fast instead of timing out.
    let started = Instant::now();
    let (success, _) = bash("exec 3<>/dev/tcp/2001:db8::9/443").await;
    assert!(!success && started.elapsed() < Duration::from_secs(1));

    eventually_async("the host to hear about the refused UDP", WAIT, || async {
        let udp = w.seen.udp.lock().unwrap();
        udp.iter()
            .any(|u| u.address == "192.0.2.9" && u.port == 9999 && u.count >= 1)
            && udp
                .iter()
                .any(|u| u.address == "198.18.77.7" && u.port == 4444)
            && udp
                .iter()
                .any(|u| u.address == "2001:db8::9" && u.port == 443)
    })
    .await;

    // A guest spraying datagrams is refused every time and reported only a few times a second.
    let spray = r#"
import socket
s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
for port in range(20000, 20300):
    try:
        s.sendto(b"x", ("192.0.2.50", port))
    except OSError:
        pass
"#;
    ok("python3", &["-c", spray]).await;
    // Absence window: gives a late report of the datagram spray time to arrive before the count is capped.
    tokio::time::sleep(Duration::from_secs(2)).await;
    let spray_reports = w
        .seen
        .udp
        .lock()
        .unwrap()
        .iter()
        .filter(|u| u.address == "192.0.2.50")
        .count();
    assert!(
        (1..=30).contains(&spray_reports),
        "300 datagrams made {spray_reports} reports"
    );
    // The first datagram of a destination is never lost to the rate limit's fairness: a later
    // quiet destination still gets through.
    ok("python3", &["-c", probe, "192.0.2.77", "7777"]).await;
    eventually_async("a later destination's report", WAIT, || async {
        w.seen
            .udp
            .lock()
            .unwrap()
            .iter()
            .any(|u| u.address == "192.0.2.77")
    })
    .await;
}

#[tokio::test]
async fn a_guest_that_deletes_or_edits_the_table_is_restored_and_reported() {
    if !enter("a_guest_that_deletes_or_edits_the_table_is_restored_and_reported").await {
        // skip-ok: the outer run re-ran this test in a namespace and asserted that it passed
        return;
    }
    let w = world("tamper").await;
    let listing = || async { ok("nft", &["list", "table", "inet", "puddle"]).await };
    let before = listing().await;
    for needle in [
        "chain nat_out",
        "type nat hook output priority dstnat - 10",
        "ip daddr 198.18.0.0/15 meta l4proto tcp redirect to :3129",
        "udp dport 53 dnat ip to 198.18.0.1:53",
        "ct status dnat accept",
        "reject with tcp reset",
        "log group 1291",
    ] {
        assert!(before.contains(needle), "missing {needle:?} in\n{before}");
    }

    ok("nft", &["delete", "table", "inet", "puddle"]).await;
    eventually_async("the table to come back", WAIT, || async {
        run(
            "nft",
            &["list", "table", "inet", "puddle"],
            Duration::from_secs(5),
        )
        .await
        .0
    })
    .await;
    eventually_async(
        "the host to be told the table was deleted",
        WAIT,
        || async {
            let told = w.seen.tampered.lock().unwrap().clone();
            told.contains(&true)
                .then_some(())
                .ok_or(format!("tampered = {told:?}"))
        },
    )
    .await;

    ok("nft", &["flush", "chain", "inet", "puddle", "filter_out"]).await;
    eventually_async(
        "the chain to be refilled and reported as edited",
        WAIT,
        || async {
            let told = w.seen.tampered.lock().unwrap().clone();
            if told.contains(&false) && listing().await.contains("filter_out") {
                Ok(())
            } else {
                Err(format!("tampered = {told:?}"))
            }
        },
    )
    .await;
    let after = listing().await;
    // Handles differ between installs; the rules do not.
    let strip = |text: &str| {
        text.lines()
            .map(|l| l.split(" # handle").next().unwrap_or(l).trim().to_owned())
            .collect::<Vec<_>>()
    };
    assert_eq!(strip(&before), strip(&after));

    // And it still captures.
    let (success, line) =
        bash(r#"exec 3<>/dev/tcp/192.0.2.7/5432; printf 'ping\n' >&3; read -t 3 l <&3; echo "$l""#)
            .await;
    assert!(success && line.trim() == "ping", "{line}");
    drop(w.agent);
}

#[tokio::test]
async fn the_rules_outlive_the_agent_and_come_back_with_the_same_stand_ins() {
    if !enter("the_rules_outlive_the_agent_and_come_back_with_the_same_stand_ins").await {
        // skip-ok: the outer run re-ran this test in a namespace and asserted that it passed
        return;
    }
    guest_network().await;
    let dir = common::TempDir::new("kill9");
    let seen = Arc::new(Seen::default());
    let socket = dir.0.join("route.sock");
    start_host(&socket, &seen);
    let table = dir.0.join("table");
    let start = || {
        let mut command = Command::new(env!("CARGO_BIN_EXE_puddle-agent"));
        command.env_clear();
        if let Some(path) = std::env::var_os("LLVM_PROFILE_FILE") {
            command.env("LLVM_PROFILE_FILE", path);
        }
        command
            .env(
                "PUDDLE_AGENT_TARGET",
                format!("unix://{}", socket.display()),
            )
            .env("PUDDLE_AGENT_OOM", "0")
            .env("PUDDLE_AGENT_BRIDGE", "off")
            .env("PUDDLE_AGENT_LISTEN", "127.0.0.1:0")
            .env("PUDDLE_AGENT_CAPTURE", "on")
            .env("PUDDLE_AGENT_DNS_TABLE", &table)
            .env("PUDDLE_AGENT_CAPTURE_POLL_MS", "100")
            .kill_on_drop(true)
            .spawn()
            .unwrap()
    };
    let mut agent = start();
    ready().await;
    let first = ok("dig", &["+short", "@198.18.0.1", "db.test"]).await;
    let handles = |text: &str| text.matches("# handle").count();
    let rules = ok("nft", &["-a", "list", "table", "inet", "puddle"]).await;
    assert!(handles(&rules) > 10, "{rules}");

    agent.kill().await.unwrap();
    agent.wait().await.unwrap();
    // Nobody is listening now, yet nothing escapes: a captured connect is refused at once.
    let started = Instant::now();
    let (success, _) = bash("exec 3<>/dev/tcp/192.0.2.7/5432").await;
    assert!(!success && started.elapsed() < Duration::from_secs(2));
    assert!(
        run(
            "nft",
            &["list", "table", "inet", "puddle"],
            Duration::from_secs(5)
        )
        .await
        .0,
        "the table is the kernel's, it stays"
    );

    let mut agent = start();
    eventually_async("the restarted agent to answer", WAIT, || async {
        run(
            "dig",
            &["+short", "+time=1", "+tries=1", "@198.18.0.1", "db.test"],
            Duration::from_secs(5),
        )
        .await
        .1 == first
    })
    .await;
    let (success, line) =
        bash(r#"exec 3<>/dev/tcp/192.0.2.7/5432; printf 'ping\n' >&3; read -t 3 l <&3; echo "$l""#)
            .await;
    assert!(success && line.trim() == "ping", "{line}");
    // Installed once more by the new agent, not stacked on the old rules.
    let again = ok("nft", &["-a", "list", "table", "inet", "puddle"]).await;
    assert_eq!(handles(&again), handles(&rules));
    agent.kill().await.unwrap();
}

#[tokio::test]
async fn capture_leaves_dockers_own_tables_alone_whichever_starts_first() {
    if !enter("capture_leaves_dockers_own_tables_alone_whichever_starts_first").await {
        // skip-ok: the outer run re-ran this test in a namespace and asserted that it passed
        return;
    }
    guest_network().await;
    // What dockerd's iptables-nft leaves behind, in kind: a nat table at the usual priority with
    // a published-port rule and masquerade.
    let docker = [
        "add table ip docker_like",
        "add chain ip docker_like PREROUTING { type nat hook prerouting priority dstnat; }",
        "add chain ip docker_like POSTROUTING { type nat hook postrouting priority srcnat; }",
        "add rule ip docker_like PREROUTING tcp dport 8080 dnat to 172.17.0.2:80",
        "add rule ip docker_like POSTROUTING ip saddr 172.17.0.0/16 oifname != docker0 masquerade",
    ];
    for line in docker {
        ok("nft", &[line]).await;
    }
    let theirs = || async { ok("nft", &["list", "table", "ip", "docker_like"]).await };
    let before = theirs().await;

    let dir = common::TempDir::new("docker");
    let seen = Arc::new(Seen::default());
    let socket = dir.0.join("route.sock");
    start_host(&socket, &seen);
    let agent = Agent::start(config(&socket, &dir.0.join("table")))
        .await
        .unwrap();
    ready().await;
    assert_eq!(
        before,
        theirs().await,
        "installing capture changed Docker's table"
    );

    // dockerd starting after capture adds its chains and rules; ours stay and still work.
    ok(
        "nft",
        &[
            "add",
            "chain",
            "ip",
            "docker_like",
            "OUTPUT",
            "{ type nat hook output priority -100; }",
        ],
    )
    .await;
    ok(
        "nft",
        &[
            "add",
            "rule",
            "ip",
            "docker_like",
            "OUTPUT",
            "ip",
            "daddr",
            "!=",
            "127.0.0.0/8",
            "return",
        ],
    )
    .await;
    // Absence window: a change to another table is not tampering, however long the watcher looks.
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(
        seen.tampered.lock().unwrap().is_empty(),
        "another table's change is not tampering"
    );
    let (success, line) =
        bash(r#"exec 3<>/dev/tcp/192.0.2.7/5432; printf 'ping\n' >&3; read -t 3 l <&3; echo "$l""#)
            .await;
    assert!(success && line.trim() == "ping", "{line}");

    // A table of the same name in another family is not ours to compare or restore.
    ok("nft", &["add", "table", "ip", "puddle"]).await;
    ok(
        "nft",
        &[
            "add",
            "chain",
            "ip",
            "puddle",
            "c",
            "{ type filter hook input priority 0; }",
        ],
    )
    .await;
    // Absence window: a change to another table is not tampering, however long the watcher looks.
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(
        seen.tampered.lock().unwrap().is_empty(),
        "an `ip puddle` table is not ours"
    );
    ok("nft", &["delete", "table", "ip", "puddle"]).await;

    // And capture rewritten (as after a restart) leaves theirs as they were.
    let theirs_now = theirs().await;
    ok("nft", &["delete", "table", "inet", "puddle"]).await;
    eventually_async("the table to come back", WAIT, || async {
        run(
            "nft",
            &["list", "table", "inet", "puddle"],
            Duration::from_secs(5),
        )
        .await
        .0
    })
    .await;
    assert_eq!(theirs_now, theirs().await);
    drop(agent);
}

#[tokio::test]
async fn with_capture_off_nothing_is_installed_and_nothing_listens() {
    if !enter("with_capture_off_nothing_is_installed_and_nothing_listens").await {
        // skip-ok: the outer run re-ran this test in a namespace and asserted that it passed
        return;
    }
    guest_network().await;
    let dir = common::TempDir::new("off");
    let seen = Arc::new(Seen::default());
    let socket = dir.0.join("route.sock");
    start_host(&socket, &seen);
    let mut config = config(&socket, &dir.0.join("table"));
    config.capture = None;
    config.dns = None;
    let agent = Agent::start(config).await.unwrap();
    // Absence window: long enough for startup to have opened a listener, had capture been on.
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert_eq!(agent.capture_addr(), None);
    assert!(
        !run(
            "nft",
            &["list", "table", "inet", "puddle"],
            Duration::from_secs(5)
        )
        .await
        .0,
        "a table appeared with capture off"
    );
    let (_, ifaces) = run("ip", &["-br", "link"], Duration::from_secs(5)).await;
    assert!(!ifaces.contains("dummy0"), "{ifaces}");
    let (success, _) = bash("exec 3<>/dev/tcp/127.0.0.1/3129").await;
    assert!(!success, "something listens on the capture port");
    assert!(seen.connects.lock().unwrap().is_empty());
    let _ = Ipv4Addr::LOCALHOST;
}

#[tokio::test]
async fn a_default_route_that_moves_gets_the_rules_rewritten_for_the_new_interface() {
    if !enter("a_default_route_that_moves_gets_the_rules_rewritten_for_the_new_interface").await {
        // skip-ok: the outer run re-ran this test in a namespace and asserted that it passed
        return;
    }
    let w = world("moved").await;
    assert!(
        ok("nft", &["list", "table", "inet", "puddle"])
            .await
            .contains("oifname \"eth0\"")
    );
    for args in [
        &["link", "add", "eth1", "type", "dummy"][..],
        &["addr", "add", "172.16.1.6/30", "dev", "eth1"],
        &["link", "set", "eth1", "up"],
        &[
            "route",
            "replace",
            "default",
            "via",
            "172.16.1.5",
            "dev",
            "eth1",
        ],
    ] {
        ok("ip", args).await;
    }
    eventually_async("the rules to follow the default route", WAIT, || async {
        let (_, text) = run(
            "nft",
            &["list", "table", "inet", "puddle"],
            Duration::from_secs(5),
        )
        .await;
        text.contains("oifname \"eth1\"") && !text.contains("oifname \"eth0\"")
    })
    .await;
    let (success, line) =
        bash(r#"exec 3<>/dev/tcp/192.0.2.7/5432; printf 'ping\n' >&3; read -t 3 l <&3; echo "$l""#)
            .await;
    assert!(success && line.trim() == "ping", "{line}");
    assert!(
        w.seen.tampered.lock().unwrap().is_empty(),
        "a network change is not tampering"
    );
}

#[tokio::test]
async fn a_guest_with_no_default_route_gets_one_through_the_dummy_interface() {
    if !enter("a_guest_with_no_default_route_gets_one_through_the_dummy_interface").await {
        // skip-ok: the outer run re-ran this test in a namespace and asserted that it passed
        return;
    }
    ok("ip", &["link", "set", "lo", "up"]).await;
    let dir = common::TempDir::new("noroute");
    let seen = Arc::new(Seen::default());
    let socket = dir.0.join("route.sock");
    start_host(&socket, &seen);
    let _agent = Agent::start(config(&socket, &dir.0.join("table")))
        .await
        .unwrap();
    ready().await;
    let routes = ok("ip", &["route", "show", "default"]).await;
    assert!(routes.contains("dev dummy0"), "{routes}");
    assert!(
        ok("nft", &["list", "table", "inet", "puddle"])
            .await
            .contains("oifname \"dummy0\"")
    );
    // Without the route this connect would fail with "network unreachable" before any rule saw it.
    let (success, line) =
        bash(r#"exec 3<>/dev/tcp/192.0.2.7/5432; printf 'ping\n' >&3; read -t 3 l <&3; echo "$l""#)
            .await;
    assert!(success && line.trim() == "ping", "{line}");
    assert!(
        seen.connects
            .lock()
            .unwrap()
            .iter()
            .any(|c| c.authority == "192.0.2.7:5432")
    );
}

#[tokio::test]
async fn with_the_host_down_a_captured_connection_is_reset_not_hung() {
    if !enter("with_the_host_down_a_captured_connection_is_reset_not_hung").await {
        // skip-ok: the outer run re-ran this test in a namespace and asserted that it passed
        return;
    }
    guest_network().await;
    let dir = common::TempDir::new("hostdown");
    // Nothing listens on the route: the agent cannot reach the host.
    let _agent = Agent::start(config(&dir.0.join("nobody.sock"), &dir.0.join("table")))
        .await
        .unwrap();
    eventually_async("the capture table", WAIT, || async {
        puddle_agent::capture::nft::read().is_ok_and(|t| t.is_some())
    })
    .await;
    let started = Instant::now();
    let (success, _) = bash("exec 3<>/dev/tcp/192.0.2.7/5432; read -t 5 l <&3").await;
    assert!(
        !success || started.elapsed() < Duration::from_secs(3),
        "the connection hung for {:?}",
        started.elapsed()
    );
}
