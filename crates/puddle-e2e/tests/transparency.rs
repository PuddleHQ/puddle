// SPDX-License-Identifier: GPL-3.0-or-later
//! The transparency bar on the product stack, no VM: traffic shaped like the heaviest real tools
//! (many parallel connections, keep-alive, huge transfers, half-close, slow readers, request
//! storms, HTTP/2) goes from a guest-side client through the real guest agent, the workspace's
//! real endpoint, the real proxy and the real rules engine to local servers.
//!
//! Every case has the same bar: no failure, and no stall over 5 s (a stall is a gap between two
//! pieces of progress on one connection, not a slow total). The gibibyte case leaves out the time
//! the kernel's own sockets on the path sat in retransmission backoff after it dropped a loopback
//! segment (runner kernels 6.16 to 6.19 do, and Puddle cannot prevent it): see `support::stall`
//! and `stall_meter.rs`, which show that a reader's pause still counts in full. Throughput is reported next to the
//! same transfer over plain loopback, never asserted: a loaded runner moves both.
#![cfg(unix)]
#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::print_stderr,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    reason = "a test fails by panicking; the measurements go to stderr, and a rate in MiB/s needs no more precision"
)]

mod support;

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use bytes::Bytes;
use h2::SendStream;
use support::stall::{Backoff, Gap, Progress, rcvq_drops};
use support::{LOCAL, allow, connect_via, echo_server, rig, round_trip};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

/// The longest a connection may make no progress.
const STALL: Duration = Duration::from_secs(5);
const GIB: u64 = 1 << 30;
const CHUNK: usize = 1 << 20;

/// One chunk of non-repeating bytes; a stream is this chunk over and over, so a receiver checks
/// every byte against the offset it is at without keeping what it has seen.
fn pattern() -> &'static [u8] {
    static PATTERN: OnceLock<Vec<u8>> = OnceLock::new();
    PATTERN.get_or_init(|| {
        let mut state = 0x9e37_79b9_7f4a_7c15_u64;
        (0..CHUNK)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state.to_le_bytes()[0]
            })
            .collect()
    })
}

/// Checks `got`, which starts `offset` bytes into the stream, against the pattern.
fn matches_stream(got: &[u8], offset: u64) -> bool {
    let mut at = usize::try_from(offset % CHUNK as u64).unwrap();
    let pattern = pattern();
    let mut rest = got;
    while !rest.is_empty() {
        let n = rest.len().min(CHUNK - at);
        if rest[..n] != pattern[at..at + n] {
            return false;
        }
        rest = &rest[n..];
        at = (at + n) % CHUNK;
    }
    true
}

fn raise_descriptors(connections: u64) {
    let limit = puddle_fd_limit::raise_open_file_limit(puddle_fd_limit::Reach::Soft);
    // Four descriptors per tunnelled connection: client, agent, proxy's upstream, server.
    assert!(
        limit.allows(4 * connections + 512),
        "this machine's open-file limit cannot hold {connections} connections through one process: {limit}"
    );
}

/// What a transfer showed.
struct Transfer {
    bytes: u64,
    took: Duration,
    /// The gap with the most time no one but the transfer's parties caused.
    worst: Gap,
}

impl Transfer {
    fn mib_per_s(&self) -> f64 {
        self.bytes as f64 / f64::from(1u32 << 20) / self.took.as_secs_f64()
    }
}

/// A server that reads to the end of the client's stream, checks it against the pattern, and
/// answers with the count and a verdict byte after the client's FIN (so it is also a half-close
/// check).
async fn counting_sink() -> SocketAddr {
    let listener = TcpListener::bind((LOCAL, 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (mut conn, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                let mut buf = vec![0u8; CHUNK];
                let (mut total, mut intact) = (0u64, true);
                loop {
                    let Ok(n) = conn.read(&mut buf).await else {
                        return;
                    };
                    if n == 0 {
                        break;
                    }
                    intact &= matches_stream(&buf[..n], total);
                    total += n as u64;
                }
                let mut reply = total.to_be_bytes().to_vec();
                reply.push(u8::from(intact));
                let _ = conn.write_all(&reply).await;
                let _ = conn.shutdown().await;
            });
        }
    });
    addr
}

/// A server that sends `bytes` of the pattern to each client, then closes its sending side.
async fn pattern_source(bytes: u64) -> SocketAddr {
    let listener = TcpListener::bind((LOCAL, 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (mut conn, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                let mut left = bytes;
                while left > 0 {
                    let n = usize::try_from(left.min(CHUNK as u64)).unwrap();
                    let Some(chunk) = pattern().get(..n) else {
                        return;
                    };
                    if conn.write_all(chunk).await.is_err() {
                        return;
                    }
                    left -= n as u64;
                }
                let _ = conn.shutdown().await;
            });
        }
    });
    addr
}

/// Sends `bytes` of the pattern to the sink at `server`, closes the sending side, and reads the
/// sink's verdict.
async fn upload(
    conn: &mut BufReader<TcpStream>,
    server: SocketAddr,
    bytes: u64,
) -> io::Result<Transfer> {
    let backoff = watch(conn, server);
    let started = Instant::now();
    let (mut sent, mut progress) = (0u64, Progress::new(&backoff));
    while sent < bytes {
        let n = usize::try_from((bytes - sent).min(CHUNK as u64)).unwrap();
        // The chunk is the same on every lap, so the sink's check holds at any offset: a lap
        // starts at a multiple of CHUNK.
        conn.get_mut().write_all(&pattern()[..n]).await?;
        sent += n as u64;
        progress.mark();
    }
    conn.get_mut().shutdown().await?;
    let mut reply = [0u8; 9];
    conn.read_exact(&mut reply).await?;
    let took = started.elapsed();
    let total = u64::from_be_bytes(reply[..8].try_into().unwrap());
    if total != bytes || reply[8] != 1 {
        return Err(io::Error::other(format!(
            "the sink saw {total} of {bytes} bytes, intact: {}",
            reply[8] == 1
        )));
    }
    Ok(Transfer {
        bytes,
        took,
        worst: progress.worst,
    })
}

/// Reads to the end of the stream from the source at `server`, checking every byte; a wait that
/// [`STALL`] passes (less the kernel's backoff) fails the transfer instead of hanging it.
async fn download(
    conn: &mut BufReader<TcpStream>,
    server: SocketAddr,
    bytes: u64,
) -> io::Result<Transfer> {
    let backoff = watch(conn, server);
    let started = Instant::now();
    let mut buf = vec![0u8; CHUNK];
    let (mut got, mut progress) = (0u64, Progress::new(&backoff));
    loop {
        let n = loop {
            tokio::select! {
                read = conn.read(&mut buf) => break read?,
                () = tokio::time::sleep(Duration::from_millis(100)) => {
                    let gap = progress.gap();
                    if gap.own() >= STALL {
                        return Err(io::Error::other(format!(
                            "no byte for {gap:?} after {got} bytes"
                        )));
                    }
                }
            }
        };
        if n == 0 {
            break;
        }
        if !matches_stream(&buf[..n], got) {
            return Err(io::Error::other(format!("bytes differ after {got}")));
        }
        got += n as u64;
        progress.mark();
    }
    if got != bytes {
        return Err(io::Error::other(format!("{got} of {bytes} bytes arrived")));
    }
    Ok(Transfer {
        bytes,
        took: started.elapsed(),
        worst: progress.worst,
    })
}

async fn direct(addr: SocketAddr) -> BufReader<TcpStream> {
    BufReader::new(TcpStream::connect(addr).await.unwrap())
}

/// Watches the sockets of one transfer on the path: the client's, the far end of its connection
/// (the agent, or the server on a direct one) and the server's port, where the proxy's own
/// connection to the server ends.
fn watch(conn: &BufReader<TcpStream>, server: SocketAddr) -> Backoff {
    let stream = conn.get_ref();
    Backoff::watch(vec![
        stream.local_addr().unwrap().port(),
        stream.peer_addr().unwrap().port(),
        server.port(),
    ])
}

fn report(what: &str, through: &Transfer, plain: &Transfer, drops_before: u64) {
    eprintln!(
        "{what}: through Puddle {:.0} MiB/s in {:.1?} (worst gap {:.2?}, of it {:.2?} kernel backoff); \
         plain loopback {:.0} MiB/s; ratio {:.2}",
        through.mib_per_s(),
        through.took,
        through.worst.raw,
        through.worst.kernel,
        plain.mib_per_s(),
        plain.mib_per_s() / through.mib_per_s(),
    );
    assert!(
        through.worst.own() < STALL,
        "{what} stalled for {:?} of its own (gap {:?}, of it {:?} the kernel's retransmission backoff; \
         the host dropped {} segments for a full receive queue meanwhile)",
        through.worst.own(),
        through.worst.raw,
        through.worst.kernel,
        rcvq_drops().saturating_sub(drops_before),
    );
}

/// A tool with more connections than any measured: 4 times pnpm's peak. Every connection sends
/// 64 KiB and gets it back, all started together.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_thousand_and_24_parallel_connects_all_succeed_with_no_stall() {
    const CONNECTIONS: usize = 1024;
    raise_descriptors(CONNECTIONS as u64);
    let rig = rig().await;
    allow(&rig.store, "echo.test");
    let echo = echo_server();
    let authority = format!("echo.test:{}", echo.port());
    let agent = rig.agent.local_addr();
    let mut set = tokio::task::JoinSet::new();
    for i in 0..CONNECTIONS {
        let authority = authority.clone();
        set.spawn(async move { round_trip(agent, &authority, i).await });
    }
    let results = tokio::time::timeout(Duration::from_secs(120), async {
        let mut out = Vec::new();
        while let Some(done) = set.join_next().await {
            out.push(done.unwrap());
        }
        out
    })
    .await
    .expect("1024 connections did not finish within 120 s");
    let failures: Vec<_> = results.iter().filter_map(|r| r.as_ref().err()).collect();
    assert!(
        failures.is_empty(),
        "{} of {CONNECTIONS} failed: {:?}",
        failures.len(),
        failures.first()
    );
    let slowest = results
        .iter()
        .filter_map(|r| r.as_ref().ok())
        .max()
        .unwrap();
    eprintln!("{CONNECTIONS} parallel CONNECT requests: slowest {slowest:?}");
    assert!(*slowest < STALL, "slowest connection took {slowest:?}");
}

/// A response with `len` bytes of body, keep-alive.
fn http_ok(len: usize) -> Vec<u8> {
    let mut out = format!("HTTP/1.1 200 OK\r\nContent-Length: {len}\r\n\r\n").into_bytes();
    out.extend(pattern().iter().copied().cycle().take(len));
    out
}

/// A small HTTP/1.1 server that answers every request on a connection, however many, with a body
/// of `(n % 1000)` bytes where `n` counts the requests so far on that connection.
async fn http_server() -> SocketAddr {
    let listener = TcpListener::bind((LOCAL, 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (conn, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                let mut conn = BufReader::new(conn);
                let mut served = 0usize;
                loop {
                    // One request: its lines up to the blank one (no body in these tests).
                    loop {
                        let mut line = String::new();
                        match conn.read_line(&mut line).await {
                            Ok(0) | Err(_) => return,
                            Ok(_) if line == "\r\n" => break,
                            Ok(_) => {}
                        }
                    }
                    if conn
                        .get_mut()
                        .write_all(&http_ok(served % 1000))
                        .await
                        .is_err()
                    {
                        return;
                    }
                    served += 1;
                }
            });
        }
    });
    addr
}

/// Reads one response and returns its body length.
async fn read_response(conn: &mut BufReader<TcpStream>) -> io::Result<usize> {
    let mut status = String::new();
    conn.read_line(&mut status).await?;
    if !status.starts_with("HTTP/1.1 200") {
        return Err(io::Error::other(format!("status line {status:?}")));
    }
    let mut len = None;
    loop {
        let mut line = String::new();
        if conn.read_line(&mut line).await? == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        if line == "\r\n" {
            break;
        }
        if let Some(value) = line.strip_prefix("Content-Length: ") {
            len = value.trim().parse().ok();
        }
    }
    let len = len.ok_or_else(|| io::Error::other("no Content-Length"))?;
    let mut body = vec![0u8; len];
    conn.read_exact(&mut body).await?;
    Ok(len)
}

/// What a package manager does with one connection: a thousand requests, one after the other.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_thousand_requests_in_a_row_on_one_keep_alive_tunnel() {
    let rig = rig().await;
    allow(&rig.store, "echo.test");
    let server = http_server().await;
    let (code, mut conn) = connect_via(
        rig.agent.local_addr(),
        &format!("echo.test:{}", server.port()),
    )
    .await
    .unwrap();
    assert_eq!(code, 200);
    let started = Instant::now();
    let mut slowest = Duration::ZERO;
    for i in 0..1000usize {
        let asked = Instant::now();
        conn.get_mut()
            .write_all(b"GET /package HTTP/1.1\r\nHost: echo.test\r\n\r\n")
            .await
            .unwrap();
        let len = read_response(&mut conn).await.unwrap();
        assert_eq!(len, i % 1000, "request {i}");
        slowest = slowest.max(asked.elapsed());
    }
    eprintln!(
        "1000 keep-alive requests: {:?} in all, slowest {slowest:?}",
        started.elapsed()
    );
    assert!(slowest < STALL, "a request took {slowest:?}");
}

/// A gibibyte up, a gibibyte down, each next to the same transfer over plain loopback.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_gibibyte_goes_up_and_a_gibibyte_comes_down_with_no_stall() {
    let rig = rig().await;
    allow(&rig.store, "echo.test");
    let sink = counting_sink().await;
    let source = pattern_source(GIB).await;
    let agent = rig.agent.local_addr();

    let drops = rcvq_drops();
    let (code, mut conn) = connect_via(agent, &format!("echo.test:{}", sink.port()))
        .await
        .unwrap();
    assert_eq!(code, 200);
    let through = upload(&mut conn, sink, GIB).await.unwrap();
    let mut plain_conn = direct(sink).await;
    let plain = upload(&mut plain_conn, sink, GIB).await.unwrap();
    report("1 GiB upload", &through, &plain, drops);

    let (code, mut conn) = connect_via(agent, &format!("echo.test:{}", source.port()))
        .await
        .unwrap();
    assert_eq!(code, 200);
    let drops = rcvq_drops();
    let through = download(&mut conn, source, GIB).await.unwrap();
    let mut plain_conn = direct(source).await;
    let plain = download(&mut plain_conn, source, GIB).await.unwrap();
    report("1 GiB download", &through, &plain, drops);
}

/// A server that answers any request with a head and `bytes` of the pattern, then closes (it sees
/// the proxy's `Connection: close`).
async fn http_source(bytes: u64) -> SocketAddr {
    let listener = TcpListener::bind((LOCAL, 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (conn, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                let mut conn = BufReader::new(conn);
                let mut line = String::new();
                while conn.read_line(&mut line).await.is_ok_and(|n| n > 2) {
                    line.clear();
                }
                let head = format!("HTTP/1.1 200 OK\r\nContent-Length: {bytes}\r\n\r\n");
                if conn.get_mut().write_all(head.as_bytes()).await.is_err() {
                    return;
                }
                let mut left = bytes;
                while left > 0 {
                    let n = usize::try_from(left.min(CHUNK as u64)).unwrap();
                    if conn.get_mut().write_all(&pattern()[..n]).await.is_err() {
                        return;
                    }
                    left -= n as u64;
                }
                let _ = conn.get_mut().shutdown().await;
            });
        }
    });
    addr
}

/// Sends `request` and reads the response head up to its body; returns the `Content-Length`.
async fn response_head(conn: &mut BufReader<TcpStream>, request: &str) -> u64 {
    conn.get_mut().write_all(request.as_bytes()).await.unwrap();
    let mut status = String::new();
    conn.read_line(&mut status).await.unwrap();
    assert_eq!(status, "HTTP/1.1 200 OK\r\n");
    let mut length = None;
    loop {
        let mut line = String::new();
        conn.read_line(&mut line).await.unwrap();
        if line == "\r\n" {
            return length.expect("a Content-Length");
        }
        if let Some(value) = line.strip_prefix("Content-Length: ") {
            length = Some(value.trim().parse().unwrap());
        }
    }
}

/// apt's way: a plain `GET http://...` with no tunnel, which the proxy forwards (reading nothing
/// more from the client once the request is in) while 256 MiB come down, sixteen stream windows.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_large_plain_http_response_comes_down_with_no_stall() {
    const SIZE: u64 = 256 << 20;
    let rig = rig().await;
    allow(&rig.store, "echo.test");
    let source = http_source(SIZE).await;
    let port = source.port();
    let mut conn = direct(rig.agent.local_addr()).await;
    let request =
        format!("GET http://echo.test:{port}/big HTTP/1.1\r\nHost: echo.test:{port}\r\n\r\n");
    assert_eq!(response_head(&mut conn, &request).await, SIZE);
    let drops = rcvq_drops();
    let through = download(&mut conn, source, SIZE).await.unwrap();
    let mut conn = direct(source).await;
    assert_eq!(
        response_head(&mut conn, "GET /big HTTP/1.1\r\n\r\n").await,
        SIZE
    );
    let plain = download(&mut conn, source, SIZE).await.unwrap();
    report("256 MiB plain-HTTP download", &through, &plain, drops);
}

/// A client that has closed its sending side still receives the whole answer, and a server that
/// has closed its sending side still receives the rest of the request (a TCP half-close is
/// something `git`, `rsync` and many protocols rely on).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_connection_closed_in_one_direction_keeps_working_in_the_other_both_ways() {
    const BYTES: u64 = 4 << 20;
    let rig = rig().await;
    allow(&rig.store, "echo.test");
    let agent = rig.agent.local_addr();

    // The client finishes first: the sink answers only after the client's FIN, which `upload`
    // waits for (and the bytes are all there).
    let sink = counting_sink().await;
    let (code, mut conn) = connect_via(agent, &format!("echo.test:{}", sink.port()))
        .await
        .unwrap();
    assert_eq!(code, 200);
    upload(&mut conn, sink, BYTES).await.unwrap();

    // The server finishes first: it sends and closes at once, and keeps reading.
    let listener = TcpListener::bind((LOCAL, 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (seen_tx, seen_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let (mut conn, _) = listener.accept().await.unwrap();
        conn.write_all(&pattern()[..CHUNK]).await.unwrap();
        conn.shutdown().await.unwrap();
        let mut got = Vec::new();
        conn.read_to_end(&mut got).await.unwrap();
        let _ = seen_tx.send(got);
    });
    let (code, mut conn) = connect_via(agent, &format!("echo.test:{port}"))
        .await
        .unwrap();
    assert_eq!(code, 200);
    let mut first = Vec::new();
    conn.read_to_end(&mut first).await.unwrap();
    assert_eq!(
        first,
        &pattern()[..CHUNK],
        "the server's data before its FIN"
    );
    let reply: Vec<u8> = pattern()
        .iter()
        .copied()
        .cycle()
        .take(BYTES as usize)
        .collect();
    conn.get_mut().write_all(&reply).await.unwrap();
    conn.get_mut().shutdown().await.unwrap();
    let seen = tokio::time::timeout(STALL, seen_rx).await.unwrap().unwrap();
    assert!(
        seen == reply,
        "the server got {} of {} bytes",
        seen.len(),
        reply.len()
    );
}

/// A download that the client drains at 1 KiB/s for 60 s: the connection stays up, the bytes
/// stay right, the proxy holds a bounded amount for it, and other connections are not slowed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_client_reading_one_kibibyte_a_second_for_a_minute_is_not_cut_off() {
    let rig = rig().await;
    allow(&rig.store, "echo.test");
    let echo = echo_server();
    let agent = rig.agent.local_addr();

    let listener = TcpListener::bind((LOCAL, 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let written = Arc::new(AtomicU64::new(0));
    let server_ended = Arc::new(AtomicU64::new(0));
    {
        let (written, server_ended) = (written.clone(), server_ended.clone());
        tokio::spawn(async move {
            let (mut conn, _) = listener.accept().await.unwrap();
            loop {
                // Endless: only a closed or cut connection stops it.
                if conn.write_all(pattern()).await.is_err() {
                    server_ended.store(1, Ordering::SeqCst);
                    return;
                }
                written.fetch_add(CHUNK as u64, Ordering::SeqCst);
            }
        });
    }
    let (code, mut slow) = connect_via(agent, &format!("echo.test:{port}"))
        .await
        .unwrap();
    assert_eq!(code, 200);

    let started = Instant::now();
    let mut got = 0u64;
    let mut buf = [0u8; 1024];
    let mut slowest_other = Duration::ZERO;
    let mut ticker = tokio::time::interval(Duration::from_secs(1));
    for second in 0..60u64 {
        ticker.tick().await;
        slow.read_exact(&mut buf).await.unwrap();
        assert!(matches_stream(&buf, got), "bytes differ after {got}");
        got += buf.len() as u64;
        if second % 5 == 0 {
            // Another connection through the same route meanwhile.
            let other = round_trip(agent, &format!("echo.test:{}", echo.port()), 1)
                .await
                .unwrap();
            slowest_other = slowest_other.max(other);
        }
    }
    let held = written.load(Ordering::SeqCst);
    eprintln!(
        "slow reader: read {got} bytes in {:?}; the server had written {} MiB; slowest other \
         connection {slowest_other:?}",
        started.elapsed(),
        held >> 20,
    );
    assert_eq!(
        server_ended.load(Ordering::SeqCst),
        0,
        "the server's connection was cut"
    );
    assert!(
        slowest_other < Duration::from_secs(1),
        "another connection waited {slowest_other:?}"
    );
    // Back pressure reaches the server: it is held at the socket buffers along the path, nowhere
    // near the 60 GiB that "as fast as it can write" would have been in a minute.
    assert!(
        held < 512 << 20,
        "the server wrote {} MiB into a reader taking 1 KiB/s",
        held >> 20
    );
}

/// A burst of short requests, each on its own tunnel, all at once: what a dependency resolver
/// that fetches every manifest in parallel does.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_storm_of_2048_requests_each_on_its_own_tunnel_all_succeed() {
    const REQUESTS: usize = 2048;
    raise_descriptors(REQUESTS as u64);
    let rig = rig().await;
    allow(&rig.store, "echo.test");
    let server = http_server().await;
    let authority = format!("echo.test:{}", server.port());
    let agent = rig.agent.local_addr();
    let mut set = tokio::task::JoinSet::new();
    for i in 0..REQUESTS {
        let authority = authority.clone();
        set.spawn(async move {
            let started = Instant::now();
            let (code, mut conn) = connect_via(agent, &authority).await?;
            if code != 200 {
                return Err(io::Error::other(format!("request {i}: status {code}")));
            }
            conn.get_mut()
                .write_all(
                    b"GET /manifest HTTP/1.1\r\nHost: echo.test\r\nConnection: close\r\n\r\n",
                )
                .await?;
            let len = read_response(&mut conn).await?;
            if len != 0 {
                return Err(io::Error::other(format!(
                    "request {i}: body of {len} bytes"
                )));
            }
            Ok(started.elapsed())
        });
    }
    let results = tokio::time::timeout(Duration::from_secs(120), async {
        let mut out = Vec::new();
        while let Some(done) = set.join_next().await {
            out.push(done.unwrap());
        }
        out
    })
    .await
    .expect("2048 requests did not finish within 120 s");
    let failures: Vec<_> = results.iter().filter_map(|r| r.as_ref().err()).collect();
    assert!(
        failures.is_empty(),
        "{} of {REQUESTS} failed: {:?}",
        failures.len(),
        failures.first()
    );
    let slowest = results
        .iter()
        .filter_map(|r| r.as_ref().ok())
        .max()
        .unwrap();
    eprintln!("{REQUESTS} request storm: slowest {slowest:?}");
    assert!(*slowest < STALL, "the slowest request took {slowest:?}");
}

/// Sends `data` on an HTTP/2 stream, honouring flow control, and ends the stream.
async fn send_all(tx: &mut SendStream<Bytes>, mut data: Bytes) -> Result<(), h2::Error> {
    while !data.is_empty() {
        tx.reserve_capacity(data.len());
        let granted = std::future::poll_fn(|cx| tx.poll_capacity(cx))
            .await
            .ok_or_else(|| h2::Error::from(h2::Reason::CANCEL))??;
        let n = granted.min(data.len());
        tx.send_data(data.split_to(n), false)?;
    }
    tx.send_data(Bytes::new(), true)
}

/// An HTTP/2 (prior knowledge, no TLS) server: `/echo` sends back the request body, `/big`
/// sends 32 MiB.
async fn h2_server() -> SocketAddr {
    let listener = TcpListener::bind((LOCAL, 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (conn, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                let Ok(mut conn) = h2::server::Builder::new()
                    .initial_window_size(1 << 20)
                    .initial_connection_window_size(16 << 20)
                    .handshake(conn)
                    .await
                else {
                    return;
                };
                while let Some(Ok((request, mut respond))) = conn.accept().await {
                    tokio::spawn(async move {
                        let (parts, mut body) = request.into_parts();
                        let mut data = Vec::new();
                        while let Some(chunk) = body.data().await {
                            let Ok(chunk) = chunk else {
                                return;
                            };
                            let _ = body.flow_control().release_capacity(chunk.len());
                            data.extend_from_slice(&chunk);
                        }
                        let data = if parts.uri.path() == "/big" {
                            Bytes::from(
                                pattern()
                                    .iter()
                                    .copied()
                                    .cycle()
                                    .take(32 << 20)
                                    .collect::<Vec<_>>(),
                            )
                        } else {
                            Bytes::from(data)
                        };
                        let response = http::Response::new(());
                        let Ok(mut tx) = respond.send_response(response, false) else {
                            return;
                        };
                        let _ = send_all(&mut tx, data).await;
                    });
                }
            });
        }
    });
    addr
}

async fn h2_exchange(
    send: h2::client::SendRequest<Bytes>,
    path: &str,
    body: Bytes,
) -> Result<(Bytes, Duration), h2::Error> {
    let started = Instant::now();
    let request = http::Request::post(format!("http://echo.test{path}"))
        .body(())
        .map_err(|_| h2::Error::from(h2::Reason::PROTOCOL_ERROR))?;
    let mut send = send.ready().await?;
    let (response, mut tx) = send.send_request(request, body.is_empty())?;
    if !body.is_empty() {
        send_all(&mut tx, body).await?;
    }
    let mut rx = response.await?.into_body();
    let mut got = Vec::new();
    while let Some(chunk) = rx.data().await {
        let chunk = chunk?;
        rx.flow_control().release_capacity(chunk.len())?;
        got.extend_from_slice(&chunk);
    }
    Ok((Bytes::from(got), started.elapsed()))
}

/// The HTTP/2 workload: a big download and `STREAMS` echoes, all multiplexed on `io`; returns the
/// time it took in all and the slowest stream.
async fn h2_workload(io: TcpStream) -> (Duration, Duration) {
    const STREAMS: usize = 200;
    let (send, connection) = h2::client::Builder::new()
        .initial_window_size(1 << 20)
        .initial_connection_window_size(16 << 20)
        .handshake(io)
        .await
        .unwrap();
    tokio::spawn(connection);
    let started = Instant::now();
    let mut set = tokio::task::JoinSet::new();
    set.spawn({
        let send = send.clone();
        async move {
            let (body, took) = h2_exchange(send, "/big", Bytes::new()).await.unwrap();
            assert_eq!(body.len(), 32 << 20);
            assert!(matches_stream(&body, 0), "the big download differs");
            took
        }
    });
    for i in 0..STREAMS {
        let send = send.clone();
        set.spawn(async move {
            let body = Bytes::from(vec![u8::try_from(i % 251).unwrap(); 128 * 1024 + i]);
            let (back, took) = h2_exchange(send, "/echo", body.clone()).await.unwrap();
            assert!(back == body, "stream {i} came back different");
            took
        });
    }
    let mut slowest = Duration::ZERO;
    while let Some(done) = tokio::time::timeout(Duration::from_secs(120), set.join_next())
        .await
        .expect("an HTTP/2 stream never finished")
    {
        slowest = slowest.max(done.unwrap());
    }
    (started.elapsed(), slowest)
}

/// What gRPC and CDNs do inside one tunnel: many multiplexed streams, with flow control, on a
/// single connection through the proxy, one of them a big download.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn many_http2_streams_share_one_tunnel_with_a_big_download_among_them() {
    let rig = rig().await;
    allow(&rig.store, "echo.test");
    let server = h2_server().await;
    let (code, conn) = connect_via(
        rig.agent.local_addr(),
        &format!("echo.test:{}", server.port()),
    )
    .await
    .unwrap();
    assert_eq!(code, 200);
    let (through, through_slowest) = h2_workload(conn.into_inner()).await;
    let (plain, plain_slowest) = h2_workload(TcpStream::connect(server).await.unwrap()).await;
    eprintln!(
        "201 HTTP/2 streams on one connection: through Puddle {through:.1?} (slowest stream \
         {through_slowest:.1?}); plain loopback {plain:.1?} (slowest {plain_slowest:.1?})"
    );
    // Everything shares one connection, so a stall of any stream is the whole run's time: a run
    // that took more than three stall limits had a stall in it (it takes 1 to 3 s on a plain loopback).
    assert!(
        through_slowest < 3 * STALL,
        "an HTTP/2 stream took {through_slowest:?} through Puddle ({plain_slowest:?} plain)"
    );
}
