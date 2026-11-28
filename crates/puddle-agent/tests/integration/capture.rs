// SPDX-License-Identifier: AGPL-3.0-or-later
//! I tests for transparent capture, with the real kernel: each test runs in its own network
//! namespace (`unshare -Urnm`, the way `real_bridge.rs` does), with a stand-in for the VM's network
//! device (`eth0`, a default route), the agent, and a fake host behind the agent's Unix socket
//! that records what reaches it. The clients are real tools with no proxy variable at all: `curl`,
//! `dig`, `bash`'s `/dev/tcp`, Python's UDP sockets.
//!
//! A test re-runs itself inside the namespace (`enter`). Where this machine cannot make a
//! namespace or lacks one of the tools (a developer's WSL), the test prints `skipped: <why>` and
//! passes; with `PUDDLE_REQUIRE_CAPTURE_NETNS` set (the Linux CI job sets it) the same
//! shortfall fails the test. `nft` is used to look at the kernel's table and to be the guest that
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
use std::time::Duration;

use puddle_agent::config::{CaptureConfig, DnsConfig, Target};
use puddle_agent::{Agent, Config};
use puddle_agent_proto::host::{
    AgentReport, GuestStream, HostConfig, StreamHandler, UdpRefusal, serve_session,
};
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

/// Why this machine cannot run the capture tests, or `None` when it can: no way to make a network
/// namespace, or a tool the tests drive (`nft`, `dig`, `curl`, `ip`, `python3`) that does not run.
fn missing_prerequisite() -> Option<String> {
    use std::sync::OnceLock;
    static MISSING: OnceLock<Option<String>> = OnceLock::new();
    MISSING
        .get_or_init(|| {
            let runs = |program: &str, args: &[&str]| {
                std::process::Command::new(program)
                    .args(args)
                    .stdin(Stdio::null())
                    .output()
                    .map_err(|err| err.to_string())
                    .and_then(|out| {
                        if out.status.success() {
                            Ok(())
                        } else {
                            Err(String::from_utf8_lossy(&out.stderr).trim().to_owned())
                        }
                    })
            };
            let namespace = runs("unshare", &["-Urnm", "true"])
                .or_else(|_| runs("sudo", &["-n", "unshare", "-nm", "true"]));
            if let Err(err) = namespace {
                return Some(format!(
                    "cannot create a network namespace (`unshare -Urnm` or passwordless `sudo unshare -nm`): {err}"
                ));
            }
            for (tool, flag) in [
                ("nft", "--version"),
                ("dig", "-v"),
                ("curl", "--version"),
                ("python3", "--version"),
                ("ip", "-V"),
            ] {
                if let Err(err) = runs(tool, &[flag]) {
                    return Some(format!("`{tool}` does not run here: {err}"));
                }
            }
            None
        })
        .clone()
}

/// Runs the calling test again inside a fresh network namespace and returns `false` (the caller
/// returns); inside the namespace returns `true` and the caller runs its body. Where the machine
/// lacks a prerequisite it returns `false` after printing why, unless `PUDDLE_REQUIRE_CAPTURE_NETNS`
/// is set, which makes that a failure.
#[expect(
    clippy::print_stderr,
    reason = "a local run without a namespace or tool says that it skipped"
)]
async fn enter(test: &str) -> bool {
    if std::env::var_os(CHILD).is_some() {
        return true;
    }
    if let Some(why) = missing_prerequisite() {
        assert!(
            std::env::var_os("PUDDLE_REQUIRE_CAPTURE_NETNS").is_none(),
            "PUDDLE_REQUIRE_CAPTURE_NETNS is set but {why}"
        );
        eprintln!("skipped: {test}: {why}");
        // nextest hides a passing test's output: a caller that wants the skips counted (the local pre-flight) names a file.
        if let Some(path) = std::env::var_os("PUDDLE_CAPTURE_SKIP_LOG") {
            use std::io::Write;
            let line = format!("{test}\t{why}\n");
            if let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
            {
                file.write_all(line.as_bytes()).unwrap();
            }
        }
        return false;
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

    fn agent_report(&self, report: AgentReport) -> impl Future<Output = ()> + Send {
        if let AgentReport::CaptureTampered { table_missing } = report {
            self.0.tampered.lock().unwrap().push(table_missing);
        }
        std::future::ready(())
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
    let (code, text) = run_code(program, args, limit).await;
    (code == Some(0), text)
}

/// [`run`] with the exit status (`None` when a signal ended the program).
async fn run_code(program: &str, args: &[&str], limit: Duration) -> (Option<i32>, String) {
    tokio::time::timeout(limit, attempt(program, args))
        .await
        .unwrap_or_else(|_| panic!("{program} {args:?} did not finish in {limit:?}"))
}

/// One run of a command with no limit of its own, for the probe of an `eventually_async` wait: the
/// wait bounds the attempt and drops it at the end (`kill_on_drop` kills the child), so a program
/// that starts late on a loaded machine is one more attempt. [`run`] would panic at its own limit
/// inside the wait's bound and never give the retry its second attempt.
async fn attempt(program: &str, args: &[&str]) -> (Option<i32>, String) {
    let output = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .unwrap_or_else(|e| panic!("{program}: {e}"));
    (
        output.status.code(),
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

/// Runs `script` and panics when it ended by waiting out a `read -t` it holds on a connection: a
/// reset or a refusal ends the read at once (status 1), a connection nobody answers ends it with
/// bash's timeout status (over 128). A connect that is never answered has no read to time out:
/// [`run`]'s own limit names it. This tells a hang from a reset by what the shell saw, not by how
/// long the machine took.
async fn bash_not_hung(script: &str) -> String {
    let (code, text) = run_code("bash", &["-c", script], Duration::from_secs(20)).await;
    assert!(
        code.is_some_and(|c| c <= 128),
        "the connection hung until the read timed out (status {code:?}): {script}\n{text}"
    );
    text
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

/// Capture is ready when the table is in and the stub answers. One slow `dig` (a process that
/// starts late on a loaded machine) is one more attempt, not a failure: `eventually_async` bounds
/// the wait and drops, and so kills, the attempt that is still running at the end of it.
async fn ready() {
    eventually_async("the capture table and the stub", WAIT, || async {
        let (_, answer) = attempt(
            "dig",
            &["+short", "+time=1", "+tries=1", "@198.18.0.1", "ready.test"],
        )
        .await;
        answer.starts_with("198.18.")
            && puddle_agent::capture::nft::read().is_ok_and(|t| t.is_some())
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

/// The world every by-name and by-address test starts from: the Docker bridge, the redirect
/// rules and the stub resolver.
async fn redirect_world() -> World {
    docker_bridge().await;
    let w = world("redirect").await;
    stub_resolver(&w.dir.0).await;
    w
}

#[tokio::test]
async fn dns_to_any_resolver_is_answered_by_the_stub_with_a_stand_in() {
    if !enter("dns_to_any_resolver_is_answered_by_the_stub_with_a_stand_in").await {
        // skip-ok: the outer run re-ran this test in a namespace and asserted that it passed
        return;
    }
    let _w = redirect_world().await;

    // DNS to any resolver is the stub's: a stand-in for a name the host allows.
    let answer = ok(
        "dig",
        &["+short", "+time=1", "+tries=1", "@8.8.8.8", "web.test"],
    )
    .await;
    assert!(answer.starts_with("198.18."), "{answer}");
}

#[tokio::test]
async fn a_tool_with_an_empty_environment_reaches_the_proxy_by_name() {
    if !enter("a_tool_with_an_empty_environment_reaches_the_proxy_by_name").await {
        // skip-ok: the outer run re-ran this test in a namespace and asserted that it passed
        return;
    }
    let w = redirect_world().await;

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
}

#[tokio::test]
async fn a_raw_tcp_client_handed_a_real_address_reaches_the_proxy_by_address() {
    if !enter("a_raw_tcp_client_handed_a_real_address_reaches_the_proxy_by_address").await {
        // skip-ok: the outer run re-ran this test in a namespace and asserted that it passed
        return;
    }
    let w = redirect_world().await;

    // A raw TCP client that was handed a real address: asked for by address.
    let (success, line) =
        bash(r#"exec 3<>/dev/tcp/192.0.2.7/5432; printf 'ping\n' >&3; read -t 3 l <&3; echo "$l""#)
            .await;
    assert!(success && line.trim() == "ping", "{line}");
    assert!(w.authorities().contains(&"192.0.2.7:5432".to_owned()));
}

#[tokio::test]
async fn a_client_that_waits_for_the_server_first_still_gets_through() {
    if !enter("a_client_that_waits_for_the_server_first_still_gets_through").await {
        // skip-ok: the outer run re-ran this test in a namespace and asserted that it passed
        return;
    }
    let w = redirect_world().await;

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
}

#[tokio::test]
async fn loopback_and_the_bridge_are_not_captured() {
    if !enter("loopback_and_the_bridge_are_not_captured").await {
        // skip-ok: the outer run re-ran this test in a namespace and asserted that it passed
        return;
    }
    let w = redirect_world().await;

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
}

#[tokio::test]
async fn dialling_the_capture_port_directly_is_closed_and_sends_nothing_up() {
    if !enter("dialling_the_capture_port_directly_is_closed_and_sends_nothing_up").await {
        // skip-ok: the outer run re-ran this test in a namespace and asserted that it passed
        return;
    }
    let w = redirect_world().await;
    let before = w.authorities().len();

    // Dialling the capture port yourself has no original destination: closed, nothing sent up.
    let port = w.agent.capture_addr().unwrap().port();
    let (_, text) = bash(&format!(
        "exec 3<>/dev/tcp/127.0.0.1/{port}; read -t 2 l <&3; echo \"got:[$l] rc=$?\""
    ))
    .await;
    assert!(!text.contains("got:[hello"), "{text}");
    assert_eq!(w.authorities().len(), before);
}

#[tokio::test]
async fn a_stand_in_nobody_handed_out_is_reset_and_nothing_is_asked_of_the_host() {
    if !enter("a_stand_in_nobody_handed_out_is_reset_and_nothing_is_asked_of_the_host").await {
        // skip-ok: the outer run re-ran this test in a namespace and asserted that it passed
        return;
    }
    let w = redirect_world().await;
    let before = w.authorities().len();

    // A stand-in nobody handed out has no name to ask for: reset, nothing asked of the host.
    bash_not_hung("exec 3<>/dev/tcp/198.18.200.1/80; read -t 3 l <&3").await;
    assert_eq!(w.authorities().len(), before);
}

#[tokio::test]
async fn a_refused_connection_is_a_reset_soon_never_a_hang() {
    if !enter("a_refused_connection_is_a_reset_soon_never_a_hang").await {
        // skip-ok: the outer run re-ran this test in a namespace and asserted that it passed
        return;
    }
    let w = redirect_world().await;

    // A refusal is a reset, soon, never a hang.
    let (code, text) = run_code(
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
    // curl's own `--max-time` ends a hang with status 28; a refusal ends it with another one.
    assert!(code.is_some_and(|c| c != 0 && c != 28), "{code:?} {text}");
    assert!(w.authorities().contains(&"x.deny.test:8080".to_owned()));
}

#[tokio::test]
async fn a_raw_ssh_client_is_recognised_from_its_first_bytes_and_refused() {
    if !enter("a_raw_ssh_client_is_recognised_from_its_first_bytes_and_refused").await {
        // skip-ok: the outer run re-ran this test in a namespace and asserted that it passed
        return;
    }
    let w = redirect_world().await;

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
        let (kind, ms) = text
            .trim()
            .split_once(' ')
            .unwrap_or_else(|| (text.trim(), "0"));
        let ms: f64 = ms.parse().unwrap_or(0.0);
        assert!(kind == "refused" || kind == "oserror", "{host}: {text}");
        assert!(ms < 50.0, "{host} took {ms} ms: {text}");
    }
    // IPv6 TCP: no capture for it, fails fast instead of timing out.
    // A connect nobody answers would wait for the kernel's own timeout, far past `run`'s limit,
    // which names it; so a failure here is the fast one.
    let (success, _) = bash("exec 3<>/dev/tcp/2001:db8::9/443").await;
    assert!(!success);

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

mod lifecycle;
