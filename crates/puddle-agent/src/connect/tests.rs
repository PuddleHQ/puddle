// SPDX-License-Identifier: AGPL-3.0-or-later
use std::task::{Context, Poll};

use puddle_test_kit::eventually;
use tokio::net::TcpListener;

use super::*;

/// A writer whose shutdown only flushes, as a process's stdout pipe behaves: the descriptor
/// stays open until the process exits.
struct KeepOpen<W>(W);

impl<W: AsyncWrite + Unpin> AsyncWrite for KeepOpen<W> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.0).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.0).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.poll_flush(cx)
    }
}

fn request(host: &str, port: u16) -> Request {
    Request::parse(&[host, &port.to_string()]).unwrap()
}

#[tokio::test]
async fn keep_open_flushes_on_shutdown_and_leaves_the_writer_open() {
    let (writer, mut peer) = tokio::io::duplex(64);
    let mut keep = KeepOpen(writer);
    keep.write_all(b"one").await.unwrap();
    keep.shutdown().await.unwrap();
    // A shut-down duplex refuses writes, so this succeeding shows shutdown left it open.
    keep.write_all(b"two").await.unwrap();
    keep.flush().await.unwrap();
    let mut got = [0u8; 6];
    peer.read_exact(&mut got).await.unwrap();
    assert_eq!(&got, b"onetwo");
}

#[test]
fn a_connect_from_a_container_carries_the_origin_header_and_one_from_the_guest_does_not() {
    let plain = String::from_utf8(connect_request("a.example:443", b"\x16\x03", false)).unwrap();
    assert_eq!(
        plain,
        "CONNECT a.example:443 HTTP/1.1\r\nHost: a.example:443\r\n\r\n\x16\x03"
    );
    let marked = String::from_utf8(connect_request("a.example:443", b"\x16\x03", true)).unwrap();
    assert_eq!(
        marked,
        "CONNECT a.example:443 HTTP/1.1\r\nHost: a.example:443\r\nx-puddle-origin: container\r\n\r\n\x16\x03"
    );
}

#[test]
fn a_request_is_a_host_a_port_and_an_optional_name() {
    let r = Request::parse(&["github.com", "22"]).unwrap();
    assert_eq!(r.authority(), "github.com:22");
    assert_eq!(
        Request::parse(&["ssh.github.com", "443", "gh"])
            .unwrap()
            .authority(),
        "ssh.github.com:443"
    );
    assert_eq!(request("::1", 22).authority(), "[::1]:22");
    assert_eq!(request("[2001:db8::1]", 22).authority(), "[2001:db8::1]:22");
    assert_eq!(request("203.0.113.7", 2222).authority(), "203.0.113.7:2222");
}

#[test]
fn anything_else_is_a_usage_error() {
    let long = "a".repeat(254);
    for args in [
        vec![],
        vec!["h"],
        vec!["h", "22", "n", "extra"],
        vec!["h", "0"],
        vec!["h", "65536"],
        vec!["h", "x"],
        vec!["", "22"],
        vec!["a b", "22"],
        vec!["a/b", "22"],
        vec!["u@h", "22"],
        vec!["h\r\nx: y", "22"],
        vec!["[::1", "22"],
        vec!["[not-an-address]", "22"],
        vec!["[::1]x", "22"],
        vec![long.as_str(), "22"],
    ] {
        let err = Request::parse(&args).unwrap_err();
        assert!(err.contains(USAGE), "{args:?}: {err}");
    }
}

#[test]
fn the_proxy_is_the_agents_listener_and_a_wildcard_means_loopback() {
    let default = Settings::from_config(&Config::default());
    assert_eq!(default.proxy, "127.0.0.1:3128".parse().unwrap());
    let wildcard = Config {
        listen: "0.0.0.0:4000".parse().unwrap(),
        ..Config::default()
    };
    assert_eq!(
        Settings::from_config(&wildcard).proxy,
        "127.0.0.1:4000".parse().unwrap()
    );
}

#[test]
fn failing_is_255_as_ssh_does() {
    assert_eq!(Exit::Done.code(), 0);
    assert_eq!(Exit::Failed.code(), 255);
}

/// A proxy that reads one request head, hands it to the test, and answers `reply`.
async fn proxy(reply: &'static [u8]) -> (SocketAddr, tokio::sync::oneshot::Receiver<Vec<u8>>) {
    let (addr, seen, _) = proxy_signalling(reply).await;
    (addr, seen)
}

/// [`proxy`], and a receiver that fires when the proxy has read the request's head.
async fn proxy_signalling(
    reply: &'static [u8],
) -> (
    SocketAddr,
    tokio::sync::oneshot::Receiver<Vec<u8>>,
    tokio::sync::oneshot::Receiver<()>,
) {
    let (head_tx, head_rx) = tokio::sync::oneshot::channel();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let (mut conn, _) = listener.accept().await.unwrap();
        let mut seen = Vec::new();
        let mut buf = [0u8; 1024];
        while !seen.windows(4).any(|w| w == b"\r\n\r\n") {
            let n = conn.read(&mut buf).await.unwrap();
            assert_ne!(n, 0, "the client closed before its request");
            seen.extend_from_slice(&buf[..n]);
        }
        let _ = head_tx.send(());
        // Give a client that sends its first bytes behind the head time to do so.
        tokio::time::sleep(Duration::from_millis(50)).await;
        let _ = tx.send(seen);
        conn.write_all(reply).await.unwrap();
        if reply.starts_with(b"HTTP/1.1 200") {
            // A tunnel to an echo server.
            let (mut r, mut w) = conn.split();
            let _ = tokio::io::copy(&mut r, &mut w).await;
        }
    });
    (addr, rx, head_rx)
}

/// No name resolves: the unit tests never ask a resolver.
fn no_names(
    _: String,
    _: u16,
    _: Duration,
) -> Pin<Box<dyn Future<Output = Option<Vec<IpAddr>>> + Send>> {
    Box::pin(async { None })
}

fn settings(proxy: SocketAddr) -> Settings {
    Settings {
        proxy,
        first_bytes: Duration::from_millis(300),
        answer: Duration::from_secs(5),
        lookup_limit: Duration::from_secs(1),
        lookup: no_names,
    }
}

/// Runs the command with `input` on stdin; returns the exit, stdout and stderr.
async fn go(settings: &Settings, request: &Request, input: &[u8]) -> (Exit, String, String) {
    let (mut ssh, stdin) = tokio::io::duplex(4096);
    let (stdout, mut out) = tokio::io::duplex(4096);
    let mut stderr = Vec::new();
    ssh.write_all(input).await.unwrap();
    ssh.shutdown().await.unwrap();
    let exit = run(settings, request, stdin, stdout, &mut stderr).await;
    let mut printed = Vec::new();
    out.read_to_end(&mut printed).await.unwrap();
    (
        exit,
        String::from_utf8(printed).unwrap(),
        String::from_utf8(stderr).unwrap(),
    )
}

#[tokio::test]
async fn an_ssh_client_is_announced_with_its_banner_behind_the_head_and_the_refusal_is_printed() {
    let (addr, seen) = proxy(
            b"HTTP/1.1 403 Forbidden\r\ncontent-length: 34\r\nx-puddle-blocked: ssh_unsupported\r\n\r\npuddle: SSH is not supported yet\n\n",
        )
        .await;
    let (exit, stdout, stderr) = go(
        &settings(addr),
        &request("github.com", 22),
        b"SSH-2.0-OpenSSH_10.0p2\r\n",
    )
    .await;
    assert_eq!(exit, Exit::Failed);
    assert_eq!(stdout, "", "nothing a client could mistake for a server");
    assert_eq!(stderr, "puddle: SSH is not supported yet\n");
    let head = String::from_utf8(seen.await.unwrap()).unwrap();
    assert_eq!(
        head,
        "CONNECT github.com:22 HTTP/1.1\r\nHost: github.com:22\r\nx-puddle-protocol: ssh\r\n\r\nSSH-2.0-OpenSSH_10.0p2\r\n"
    );
}

#[tokio::test]
async fn anything_else_is_carried_both_ways_with_no_announcement() {
    let (addr, seen, head_read) =
        proxy_signalling(b"HTTP/1.1 200 Connection Established\r\n\r\nserver says hi\n").await;
    let (mut client, stdin) = tokio::io::duplex(64);
    let (stdout, mut out) = tokio::io::duplex(64);
    let mut stderr = Vec::new();
    let task = tokio::spawn(async move {
        let exit = run(
            &settings(addr),
            &request("db.test", 5432),
            stdin,
            stdout,
            &mut stderr,
        )
        .await;
        (exit, stderr)
    });
    client.write_all(b"hello").await.unwrap();
    // The proxy has the request, so the bridge took "hello" as the first bytes already.
    head_read.await.unwrap();
    client.write_all(b" later").await.unwrap();
    client.shutdown().await.unwrap();
    let (exit, stderr) = task.await.unwrap();
    assert_eq!((exit, stderr.as_slice()), (Exit::Done, &b""[..]));
    // What the proxy sent behind its answer comes first, then the echo of the later upload;
    // the first bytes went with the request.
    let mut printed = Vec::new();
    out.read_to_end(&mut printed).await.unwrap();
    assert_eq!(printed, b"server says hi\n later");
    let head = String::from_utf8(seen.await.unwrap()).unwrap();
    assert_eq!(
        head,
        "CONNECT db.test:5432 HTTP/1.1\r\nHost: db.test:5432\r\n\r\nhello"
    );
}

/// What an `ssh` `ProxyCommand` does: its end of both pipes stays open until the command exits
/// (`stdout` does not even close on shutdown), so only the server's close can end the command.
#[tokio::test]
async fn a_server_that_closes_ends_the_command_though_the_client_keeps_its_pipes_open() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut conn, _) = listener.accept().await.unwrap();
        // Everything the client sent is read, so the close is a FIN and not a reset.
        let mut seen = Vec::new();
        let mut buf = [0u8; 256];
        while !seen.ends_with(b"OpenSSH_10.0p2\r\n") {
            let n = conn.read(&mut buf).await.unwrap();
            assert_ne!(n, 0, "the client closed before its banner");
            seen.extend_from_slice(&buf[..n]);
        }
        conn.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\nSSH-2.0-fixture\r\n")
            .await
            .unwrap();
        // Dropped here: the server closed after its banner.
    });
    let (mut ssh, stdin) = tokio::io::duplex(64);
    ssh.write_all(b"SSH-2.0-OpenSSH_10.0p2\r\n").await.unwrap();
    let (stdout, mut out) = tokio::io::duplex(64);
    let task = tokio::spawn(async move {
        let mut stderr = Vec::new();
        run(
            &settings(addr),
            &request("git.example.test", 22),
            stdin,
            KeepOpen(stdout),
            &mut stderr,
        )
        .await
    });
    eventually(
        "the command ends after the server closed",
        Duration::from_secs(10),
        || task.is_finished(),
    )
    .await;
    assert_eq!(task.await.unwrap(), Exit::Done);
    let mut printed = [0u8; 17];
    out.read_exact(&mut printed).await.unwrap();
    assert_eq!(&printed, b"SSH-2.0-fixture\r\n");
    drop(ssh);
}

/// An SSH server sends its KEXINIT right behind its banner with no round trip, so both can
/// come in the read that holds the proxy's answer; the server then waits for the client.
/// The process's stdout is line-buffered (a `BufWriter` here), so what came behind the answer
/// reaches the client only if it is flushed.
#[tokio::test]
async fn what_the_server_sent_behind_the_answer_reaches_a_buffered_stdout_while_it_waits() {
    const AHEAD: &[u8] = b"SSH-2.0-fixture\r\n\x00\x00\x00\x2c\x06\x14KEXINIT";
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        let (mut conn, _) = listener.accept().await.unwrap();
        let mut seen = Vec::new();
        let mut buf = [0u8; 256];
        while !seen.ends_with(b"OpenSSH_10.0p2\r\n") {
            let n = conn.read(&mut buf).await.unwrap();
            assert_ne!(n, 0, "the client closed before its banner");
            seen.extend_from_slice(&buf[..n]);
        }
        let mut answer = b"HTTP/1.1 200 Connection Established\r\n\r\n".to_vec();
        answer.extend_from_slice(AHEAD);
        conn.write_all(&answer).await.unwrap();
        // The server waits for the client's key exchange and sends nothing more.
        let _ = done_rx.await;
    });
    let (mut ssh, stdin) = tokio::io::duplex(64);
    ssh.write_all(b"SSH-2.0-OpenSSH_10.0p2\r\n").await.unwrap();
    let (stdout, mut out) = tokio::io::duplex(256);
    let task = tokio::spawn(async move {
        let mut stderr = Vec::new();
        run(
            &settings(addr),
            &request("git.example.test", 22),
            stdin,
            tokio::io::BufWriter::new(stdout),
            &mut stderr,
        )
        .await
    });
    let mut printed = vec![0u8; AHEAD.len()];
    tokio::time::timeout(Duration::from_secs(10), out.read_exact(&mut printed))
        .await
        .expect("the bytes behind the answer never reached stdout")
        .unwrap();
    assert_eq!(printed, AHEAD);
    let _ = done_tx.send(());
    drop(ssh);
    let _ = task.await;
}

#[tokio::test]
async fn an_ssh_client_that_went_away_ends_the_command_with_a_failure() {
    // The proxy opens the tunnel and sends something behind its answer; stdout is gone.
    let (addr, _) = proxy(b"HTTP/1.1 200 Connection Established\r\n\r\nserver says hi\n").await;
    let (_ssh, stdin) = tokio::io::duplex(64);
    let (stdout, out) = tokio::io::duplex(64);
    drop(out);
    let mut stderr = Vec::new();
    let exit = run(
        &settings(addr),
        &request("db.test", 5432),
        stdin,
        stdout,
        &mut stderr,
    )
    .await;
    assert_eq!(exit, Exit::Failed);
}

#[tokio::test]
async fn a_client_that_waits_for_the_server_is_carried_too() {
    // Nothing on stdin within the first-bytes time: no banner, so no announcement.
    let (addr, seen) = proxy(b"HTTP/1.1 200 Connection Established\r\n\r\n").await;
    let (mut keep, stdin) = tokio::io::duplex(64);
    let (stdout, mut out) = tokio::io::duplex(64);
    let mut stderr = Vec::new();
    let task = tokio::spawn(async move {
        let exit = run(
            &settings(addr),
            &request("smtp.test", 25),
            stdin,
            stdout,
            &mut stderr,
        )
        .await;
        (exit, stderr)
    });
    // Absence window: stdin stays silent past the first-bytes time (300 ms), so no banner is announced.
    tokio::time::sleep(Duration::from_millis(500)).await;
    keep.write_all(b"EHLO x\r\n").await.unwrap();
    keep.shutdown().await.unwrap();
    let (exit, stderr) = task.await.unwrap();
    assert_eq!((exit, stderr.as_slice()), (Exit::Done, &b""[..]));
    let mut echoed = Vec::new();
    out.read_to_end(&mut echoed).await.unwrap();
    assert_eq!(echoed, b"EHLO x\r\n");
    let head = String::from_utf8(seen.await.unwrap()).unwrap();
    assert!(!head.contains("x-puddle-protocol"), "{head}");
}

#[tokio::test]
async fn a_refusal_without_a_body_names_the_status() {
    let (addr, _) = proxy(b"HTTP/1.1 502 Bad Gateway\r\ncontent-length: 0\r\n\r\n").await;
    let (exit, _, stderr) = go(&settings(addr), &request("a.test", 22), b"x").await;
    assert_eq!(exit, Exit::Failed);
    assert_eq!(stderr, "puddle: the proxy answered 502\n");
}

#[tokio::test]
async fn a_body_without_a_length_is_read_to_its_end_and_a_long_one_is_cut() {
    let (addr, _) = proxy(b"HTTP/1.1 403 Forbidden\r\n\r\npuddle: no length\n").await;
    let (_, _, stderr) = go(&settings(addr), &request("a.test", 22), b"x").await;
    assert_eq!(stderr, "puddle: no length\n");
    let long = format!(
        "HTTP/1.1 403 Forbidden\r\ncontent-length: 10000\r\n\r\n{}",
        "x".repeat(10_000)
    );
    let (addr, _) = proxy(Box::leak(long.into_bytes().into_boxed_slice())).await;
    let (_, _, stderr) = go(&settings(addr), &request("a.test", 22), b"x").await;
    assert_eq!(
        stderr.len(),
        MAX_BODY + 1,
        "cut at {MAX_BODY} bytes, plus the newline"
    );
}

#[tokio::test]
async fn trouble_with_the_proxy_is_said_on_stderr() {
    // Nothing listens.
    let gone = {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        listener.local_addr().unwrap()
    };
    let (exit, stdout, stderr) = go(&settings(gone), &request("a.test", 22), b"x").await;
    assert_eq!((exit, stdout.as_str()), (Exit::Failed, ""));
    assert!(
        stderr.starts_with(&format!("puddle-agent connect: a.test:22 through {gone}: ")),
        "{stderr}"
    );
    // An answer that is not HTTP, one that ends early, and one that never comes.
    for (reply, why) in [
        (&b"SSH-2.0-what\r\n\r\n"[..], "not HTTP"),
        (b"HTTP/1.1 4", "closed the connection"),
    ] {
        let (addr, _) = proxy(Box::leak(reply.to_vec().into_boxed_slice())).await;
        let (exit, _, stderr) = go(&settings(addr), &request("a.test", 22), b"x").await;
        assert_eq!(exit, Exit::Failed);
        assert!(stderr.contains(why), "{why}: {stderr}");
    }
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let silent = listener.local_addr().unwrap();
    let silent_proxy = tokio::spawn(async move {
        // Takes the connection and never answers, until the client gives up.
        let (mut conn, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 1024];
        while conn.read(&mut buf).await.unwrap_or(0) > 0 {}
    });
    let quick = Settings {
        answer: Duration::from_millis(150),
        ..settings(silent)
    };
    let (exit, _, stderr) = go(&quick, &request("a.test", 22), b"x").await;
    assert_eq!(exit, Exit::Failed);
    assert!(stderr.contains("timed out"), "{stderr}");
    silent_proxy.await.unwrap();
}

/// A few names with fixed answers, the rest do not resolve.
fn names(
    host: String,
    _: u16,
    _: Duration,
) -> Pin<Box<dyn Future<Output = Option<Vec<IpAddr>>> + Send>> {
    let ip = |text: &str| text.parse::<IpAddr>().unwrap();
    Box::pin(async move {
        match host.as_str() {
            "app.test" => Some(vec![ip("127.0.0.1")]),
            "bridge.test" => Some(vec![ip("172.17.0.5")]),
            // A DNS name that starts with 127. is just a name: its zone says where it goes.
            "127.0.0.1.nip.test" | "127.evil.test" => Some(vec![ip("203.0.113.9")]),
            "mixed.test" => Some(vec![ip("127.0.0.1"), ip("203.0.113.9")]),
            "empty.test" => Some(vec![]),
            _ => None,
        }
    })
}

fn bridge_request(host: &str, port: u16) -> Request {
    Request::parse(&["--local-net", "172.17.0.0/16", host, &port.to_string()]).unwrap()
}

#[test]
fn the_workspace_is_loopback_and_the_listed_networks_by_address() {
    let r = bridge_request("h", 22);
    for local in [
        "127.0.0.1",
        "127.255.0.9",
        "::1",
        "::ffff:127.0.0.1",
        "0.0.0.0",
        "::",
        "172.17.0.5",
        "172.17.255.255",
    ] {
        assert!(r.is_local(local.parse().unwrap()), "{local}");
    }
    for outside in [
        "203.0.113.9",
        "172.18.0.5",
        "172.16.255.255",
        "10.0.0.1",
        "2001:db8::1",
        "fe80::1",
        "::ffff:203.0.113.9",
    ] {
        assert!(!r.is_local(outside.parse().unwrap()), "{outside}");
    }
    // Without a network listed only loopback is inside.
    assert!(!request("h", 22).is_local("172.17.0.5".parse().unwrap()));
}

#[test]
fn networks_are_checked_when_parsed() {
    let r = Request::parse(&[
        "--local-net",
        "10.1.2.3/8",
        "--local-net",
        "192.168.0.0/16",
        "h",
        "22",
        "n",
    ])
    .unwrap();
    assert!(r.is_local("10.200.0.1".parse().unwrap()));
    assert!(r.is_local("192.168.9.9".parse().unwrap()));
    assert!(!r.is_local("192.169.0.1".parse().unwrap()));
    assert!(
        Request::parse(&["--local-net", "0.0.0.0/32", "h", "22"])
            .unwrap()
            .is_local("0.0.0.0".parse().unwrap())
    );
    for bad in [
        "172.17.0.0",
        "172.17.0.0/0",
        "172.17.0.0/33",
        "nope/16",
        "172.17.0.0/x",
        "::1/64",
    ] {
        let err = Request::parse(&["--local-net", bad, "h", "22"]).unwrap_err();
        assert!(
            err.contains("bad network") && err.contains(USAGE),
            "{bad}: {err}"
        );
    }
    assert!(Request::parse(&["--local-net"]).is_err());
}

/// An echo server on loopback; the port.
async fn echo() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let (mut conn, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                let (mut r, mut w) = conn.split();
                let _ = tokio::io::copy(&mut r, &mut w).await;
            });
        }
    });
    port
}

/// A proxy address nobody listens on: a connection that goes there fails loudly.
async fn no_proxy() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    listener.local_addr().unwrap()
}

#[tokio::test]
async fn an_address_inside_the_workspace_is_dialled_directly_and_never_asked_of_the_proxy() {
    let port = echo().await;
    let direct = Settings {
        lookup: names,
        ..settings(no_proxy().await)
    };
    // A literal address, and a name that resolves to loopback.
    for host in ["127.0.0.1", "::ffff:127.0.0.1", "app.test"] {
        let (exit, stdout, stderr) =
            go(&direct, &bridge_request(host, port), b"SSH-2.0-x\r\n").await;
        assert_eq!(
            (exit, stdout.as_str(), stderr.as_str()),
            (Exit::Done, "SSH-2.0-x\r\n", ""),
            "{host}"
        );
    }
}

#[tokio::test]
async fn a_dns_name_that_starts_with_127_goes_to_the_proxy_unless_it_resolves_inside() {
    for host in [
        "127.0.0.1.nip.test",
        "127.evil.test",
        "mixed.test",
        "empty.test",
        "nowhere.test",
    ] {
        let (addr, seen) = proxy(b"HTTP/1.1 403 Forbidden\r\ncontent-length: 3\r\n\r\nno\n").await;
        let through = Settings {
            lookup: names,
            ..settings(addr)
        };
        let (exit, _, stderr) = go(&through, &bridge_request(host, 22), b"SSH-2.0-x\r\n").await;
        assert_eq!((exit, stderr.as_str()), (Exit::Failed, "no\n"), "{host}");
        let head = String::from_utf8(seen.await.unwrap()).unwrap();
        assert!(
            head.starts_with(&format!("CONNECT {host}:22 HTTP/1.1")),
            "{head}"
        );
    }
}

#[tokio::test]
async fn a_local_destination_that_refuses_is_said_on_stderr() {
    let closed = {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        listener.local_addr().unwrap().port()
    };
    let direct = Settings {
        lookup: names,
        ..settings(no_proxy().await)
    };
    let (exit, stdout, stderr) = go(&direct, &bridge_request("app.test", closed), b"x").await;
    assert_eq!((exit, stdout.as_str()), (Exit::Failed, ""));
    assert!(
        stderr.starts_with(&format!("puddle-agent connect: 127.0.0.1 port {closed}: ")),
        "{stderr}"
    );
}

#[tokio::test]
async fn a_direct_connection_whose_client_went_away_fails() {
    let port = echo().await;
    let direct = Settings {
        lookup: names,
        ..settings(no_proxy().await)
    };
    let (mut ssh, stdin) = tokio::io::duplex(64);
    let (stdout, out) = tokio::io::duplex(64);
    drop(out);
    ssh.write_all(b"x").await.unwrap();
    let mut stderr = Vec::new();
    let request = bridge_request("127.0.0.1", port);
    let exit = run(&direct, &request, stdin, stdout, &mut stderr).await;
    assert_eq!(exit, Exit::Failed);
}

#[tokio::test]
async fn the_system_resolver_answers_with_addresses_and_gives_none_for_a_name_it_cannot_find() {
    let limit = Duration::from_secs(10);
    let found = system_lookup("localhost".to_owned(), 22, limit)
        .await
        .unwrap();
    assert!(found.iter().all(IpAddr::is_loopback), "{found:?}");
    assert_eq!(
        system_lookup("bad host.invalid".to_owned(), 22, limit).await,
        None
    );
}

fn never_answers(
    _: String,
    _: u16,
    limit: Duration,
) -> Pin<Box<dyn Future<Output = Option<Vec<IpAddr>>> + Send>> {
    Box::pin(within(
        limit,
        std::future::pending::<io::Result<std::vec::IntoIter<SocketAddr>>>(),
    ))
}

#[tokio::test]
async fn a_resolver_that_never_answers_gives_up_at_the_limit_and_the_name_is_not_local() {
    let limit = Duration::from_millis(50);
    let started = std::time::Instant::now();
    assert_eq!(never_answers(String::new(), 22, limit).await, None);
    assert!(started.elapsed() >= limit);
    // An answer that is in time is kept, a failed one is no answer.
    let one: SocketAddr = "127.0.0.1:22".parse().unwrap();
    let ready = std::future::ready(Ok(vec![one].into_iter()));
    assert_eq!(
        within(limit, ready).await,
        Some(vec![IpAddr::from([127, 0, 0, 1])])
    );
    let failed = std::future::ready(Err::<std::vec::IntoIter<SocketAddr>, _>(io::Error::other(
        "no",
    )));
    assert_eq!(within(limit, failed).await, None);
    // The command asks with the limit the settings carry; without an answer the name is not local.
    let config = Config::default();
    assert_eq!(
        Settings::from_config(&config).lookup_limit,
        Duration::from_secs(2)
    );
    let hanging = Settings {
        lookup_limit: limit,
        lookup: never_answers,
        ..settings(SocketAddr::from(([127, 0, 0, 1], 9)))
    };
    let request = bridge_request("slow.test", 22);
    assert!(inside_the_workspace(&hanging, &request).await.is_none());
}
