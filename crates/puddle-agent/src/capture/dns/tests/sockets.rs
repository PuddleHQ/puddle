// SPDX-License-Identifier: AGPL-3.0-or-later
//! The stub over real sockets, and its reports to the host.

use super::*;

// ---- the sockets ----

fn loopback() -> SocketAddr {
    "127.0.0.1:0".parse().unwrap()
}

async fn serve<F, Fut>(f: F, limits: Limits) -> (DnsServer, Arc<Fake<F>>)
where
    F: Fn(ResolveQuery) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<ResolveAnswer, ResolveError>> + Send + 'static,
{
    let (stub, fake, _) = stub_with(f, limits);
    (DnsServer::bind(stub, loopback()).await.unwrap(), fake)
}

async fn udp_query(server: &DnsServer, message: &[u8]) -> Option<Vec<u8>> {
    let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    socket.send_to(message, server.udp_addr()).await.unwrap();
    let mut buf = vec![0u8; 4096];
    match tokio::time::timeout(Duration::from_millis(300), socket.recv_from(&mut buf)).await {
        Ok(Ok((n, _))) => Some(buf[..n].to_vec()),
        _ => None,
    }
}

async fn tcp_exchange(stream: &mut tokio::net::TcpStream, message: &[u8]) -> Option<Vec<u8>> {
    let mut framed = u16::try_from(message.len()).unwrap().to_be_bytes().to_vec();
    framed.extend_from_slice(message);
    stream.write_all(&framed).await.ok()?;
    let mut len = [0u8; 2];
    tokio::time::timeout(WAIT, stream.read_exact(&mut len))
        .await
        .ok()?
        .ok()?;
    let mut answer = vec![0u8; usize::from(u16::from_be_bytes(len))];
    stream.read_exact(&mut answer).await.ok()?;
    Some(answer)
}

#[tokio::test]
async fn the_server_answers_over_udp_and_over_tcp_on_one_connection() {
    let (server, _) = serve(|_| std::future::ready(Ok(stand_in())), Limits::default()).await;
    let udp = udp_query(
        &server,
        &query_bytes(1, "udp.example.com", TYPE_A, Some(1232)),
    )
    .await
    .unwrap();
    let r = decode(&udp);
    assert_eq!((r.id, r.rcode(), r.answers.len()), (1, 0, 1));
    assert_eq!(r.additional.len(), 1, "the OPT echo");
    let mut tcp = tokio::net::TcpStream::connect(server.tcp_addr())
        .await
        .unwrap();
    for i in 0..3u16 {
        let answer = tcp_exchange(&mut tcp, &query_bytes(i, "tcp.example.com", TYPE_A, None))
            .await
            .unwrap();
        let r = decode(&answer);
        assert_eq!((r.id, r.answers.len()), (i, 1));
    }
    assert_ne!(server.udp_addr().port(), 0);
}

#[tokio::test]
async fn a_large_answer_is_truncated_over_udp_and_whole_over_tcp() {
    let (server, _) = serve(
        |_| {
            std::future::ready(Ok(ResolveAnswer::Records {
                records: vec![HostRecord::Txt {
                    strings: vec!["z".repeat(2000)],
                }],
                ttl: 30,
            }))
        },
        Limits::default(),
    )
    .await;
    let q = query_bytes(5, "big.example.com", TYPE_TXT, None);
    let udp = decode(&udp_query(&server, &q).await.unwrap());
    assert!(udp.truncated());
    assert!(udp.answers.is_empty());
    assert!(udp.len <= 512);
    let mut tcp = tokio::net::TcpStream::connect(server.tcp_addr())
        .await
        .unwrap();
    let whole = decode(&tcp_exchange(&mut tcp, &q).await.unwrap());
    assert!(!whole.truncated());
    assert_eq!(whole.answers.len(), 1);
}

#[tokio::test]
async fn bad_udp_and_tcp_input_gets_an_error_or_nothing_and_the_server_carries_on() {
    let (server, fake) = serve(|_| std::future::ready(Ok(stand_in())), Limits::default()).await;
    assert!(udp_query(&server, &[0xFF; 5]).await.is_none());
    let mut two = query_bytes(3, "example.com", TYPE_A, None);
    two[5] = 2;
    let r = decode(&udp_query(&server, &two).await.unwrap());
    assert_eq!(r.rcode(), 1);
    // TCP: a zero length, an over-long length and a cut message each end the connection.
    for first in [vec![0u8, 0], vec![0xFF, 0xFF], vec![0, 40, 1, 2]] {
        let mut tcp = tokio::net::TcpStream::connect(server.tcp_addr())
            .await
            .unwrap();
        tcp.write_all(&first).await.unwrap();
        if first.len() > 2 {
            tcp.shutdown().await.unwrap();
        }
        let mut sink = Vec::new();
        let read = tokio::time::timeout(WAIT, tcp.read_to_end(&mut sink))
            .await
            .unwrap();
        assert!(read.is_ok() || read.is_err());
        assert_eq!(sink, Vec::<u8>::new());
    }
    let ok = udp_query(&server, &query_bytes(4, "example.com", TYPE_A, None))
        .await
        .unwrap();
    assert_eq!(decode(&ok).answers.len(), 1);
    assert_eq!(fake.asks.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn an_idle_tcp_connection_is_closed() {
    let (server, _) = serve(
        |_| std::future::ready(Ok(stand_in())),
        Limits {
            tcp_idle: Duration::from_secs(3),
            ..Limits::default()
        },
    )
    .await;
    let mut tcp = tokio::net::TcpStream::connect(server.tcp_addr())
        .await
        .unwrap();
    let mut buf = [0u8; 1];
    let n = tokio::time::timeout(Duration::from_secs(10), tcp.read(&mut buf))
        .await
        .expect("the server closes an idle connection");
    assert_eq!(n.unwrap(), 0);
}

#[tokio::test]
async fn tcp_connections_over_the_limit_are_closed() {
    let (server, _) = serve(
        |_| std::future::ready(Ok(stand_in())),
        Limits {
            max_tcp_connections: 2,
            ..Limits::default()
        },
    )
    .await;
    let mut held = Vec::new();
    for _ in 0..2 {
        let mut c = tokio::net::TcpStream::connect(server.tcp_addr())
            .await
            .unwrap();
        assert!(
            tcp_exchange(&mut c, &query_bytes(1, "a.example.com", TYPE_A, None))
                .await
                .is_some()
        );
        held.push(c);
    }
    let mut third = tokio::net::TcpStream::connect(server.tcp_addr())
        .await
        .unwrap();
    assert!(
        tcp_exchange(&mut third, &query_bytes(1, "a.example.com", TYPE_A, None))
            .await
            .is_none()
    );
}

#[tokio::test]
async fn udp_queries_over_the_limit_are_dropped_and_the_stub_carries_on() {
    let (server, fake) = serve(
        |_| async {
            // The first query stays in flight (real time: a socket test) while the second arrives.
            tokio::time::sleep(Duration::from_millis(150)).await;
            Ok(stand_in())
        },
        Limits {
            max_udp_queries: 1,
            ..Limits::default()
        },
    )
    .await;
    let message = query_bytes(1, "a.example.com", TYPE_A, None);
    let (first, second) = tokio::join!(udp_query(&server, &message), async {
        // Arrives once the first is being answered.
        eventually(
            "the first query reached the host",
            Duration::from_secs(10),
            || fake.asks.load(Ordering::SeqCst) != 0,
        )
        .await;
        udp_query(&server, &message).await
    });
    assert!(first.is_some(), "the query that fit is answered");
    assert!(second.is_none(), "the one over the limit is dropped");
    assert!(
        udp_query(&server, &message).await.is_some(),
        "the stub still answers afterwards"
    );
}

#[tokio::test(start_paused = true)]
async fn a_flood_of_drops_is_one_log_line_every_ten_seconds_with_the_count() {
    let mut notice = DropNotice::new("queries dropped");
    let start = Instant::now();
    assert_eq!(
        notice.count_one(start),
        Some(1),
        "the first is said at once"
    );
    for _ in 0..99 {
        assert_eq!(notice.count_one(start + Duration::from_secs(1)), None);
    }
    assert_eq!(
        notice.count_one(start + Duration::from_secs(10)),
        Some(100),
        "ten seconds later: the count since the last line"
    );
    notice.note(start + Duration::from_secs(11));
}

/// A loopback port held on both UDP and TCP, below the range the OS picks from for port 0 and for
/// outgoing connections (Linux 32768-60999, Windows and macOS 49152-65535). A port 0 address is
/// only UDP's: its TCP side may be in use elsewhere, and another process asking for any port can
/// take it between the test letting it go and the server binding it.
fn held_port() -> (std::net::UdpSocket, std::net::TcpListener, SocketAddr) {
    let first = u16::try_from(std::process::id() % 10_000).unwrap_or(0);
    (0..10_000u16)
        .map(|i| SocketAddr::from(([127, 0, 0, 1], 20_000 + (first + i) % 10_000)))
        .find_map(|addr| {
            let udp = std::net::UdpSocket::bind(addr).ok()?;
            Some((udp, std::net::TcpListener::bind(addr).ok()?, addr))
        })
        .expect("a loopback port free on both UDP and TCP in 20000-29999")
}

#[tokio::test]
async fn binding_waits_for_an_address_that_is_taken_and_then_serves() {
    let (udp, tcp, listen) = held_port();
    let (stub, _, _) = answers_with(stand_in());
    assert!(DnsServer::bind(stub.clone(), listen).await.is_err());
    let task = tokio::spawn(bind_when_ready(stub, listen));
    // Absence window on a paused clock: five retry intervals pass, so the bind has been tried
    // again and still waits. The clock runs again for the real sockets below.
    tokio::time::pause();
    tokio::time::sleep(Duration::from_millis(5200)).await;
    assert!(!task.is_finished());
    drop((udp, tcp));
    // One more retry interval (virtual): the next try finds the address free.
    tokio::time::sleep(Duration::from_millis(1200)).await;
    tokio::time::resume();
    let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    // The server needs both UDP and TCP on `listen`: once it is up, a query is answered.
    eventually_async(
        "the stub answers a query on the freed address",
        Duration::from_secs(10),
        || async {
            socket
                .send_to(&query_bytes(7, "x.example.com", TYPE_A, None), listen)
                .await
                .unwrap();
            let mut buf = vec![0u8; 1024];
            matches!(
                tokio::time::timeout(Duration::from_millis(100), socket.recv_from(&mut buf)).await,
                Ok(Ok((n, _))) if decode(&buf[..n]).answers.len() == 1
            )
        },
    )
    .await;
    task.abort();
}

#[tokio::test]
async fn asking_through_an_unreachable_host_is_an_error_not_a_hang() {
    let upstream = Arc::new(Upstream::new(&crate::Config {
        target: crate::config::Target::Unix("/nonexistent/puddle-route.sock".into()),
        ..crate::Config::default()
    }));
    let ask = HostAsk::new(upstream, Duration::from_secs(2));
    let result = ask
        .ask(ResolveQuery::new("example.com", RecordType::A))
        .await;
    assert!(result.is_err());
}

#[test]
fn only_answers_of_the_asked_kind_are_kept() {
    for (rtype, answer, kept) in [
        (RecordType::A, stand_in(), true),
        (RecordType::A, ResolveAnswer::NoData { ttl: 1 }, true),
        (
            RecordType::A,
            ResolveAnswer::Records {
                records: vec![],
                ttl: 1,
            },
            false,
        ),
        (RecordType::Txt, stand_in(), false),
        (RecordType::Mx, ResolveAnswer::NoSuchName { ttl: 1 }, true),
        (RecordType::Srv, ResolveAnswer::Unknown, false),
    ] {
        let fitted = fit_to(rtype, answer.clone());
        assert_eq!(fitted == answer, kept, "{rtype:?} {answer:?}");
        if !kept {
            assert_eq!(fitted, ResolveAnswer::Unavailable);
        }
    }
}

/// The kernel drops what does not fit its receive queue, and a dropped query costs its client a
/// five-second attempt: the queue must hold the burst the stub promises to handle.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn the_udp_receive_queue_is_sized_for_the_queries_the_stub_handles_at_once() {
    let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let default = rustix::net::sockopt::socket_recv_buffer_size(&socket).unwrap();
    make_room_for_a_burst(&socket, 1024);
    let now = rustix::net::sockopt::socket_recv_buffer_size(&socket).unwrap();
    // Without privilege the kernel caps the request at `rmem_max` (and reports it doubled).
    let cap: usize = std::fs::read_to_string("/proc/sys/net/core/rmem_max")
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let asked = 1024 * UDP_QUEUE_BYTES_PER_QUERY;
    assert!(
        now >= default && now >= 2 * cap.min(asked),
        "receive queue {now} bytes (default {default}, cap {cap})"
    );
}

// ---- reports to the host ----

#[test]
fn a_tally_reports_the_first_of_a_kind_at_once_then_a_count_every_ten_seconds() {
    let mut tally = Tally::default();
    let t0 = Instant::now();
    let kind = StubDnsTrouble::LookupFailed;
    assert_eq!(tally.count(kind, t0), Some(1));
    assert_eq!(tally.count(kind, t0 + Duration::from_secs(1)), None);
    assert_eq!(tally.count(kind, t0 + Duration::from_secs(2)), None);
    // Another kind has its own window.
    assert_eq!(tally.count(StubDnsTrouble::UdpDropped, t0), Some(1));
    assert_eq!(tally.count(kind, t0 + Tally::EVERY), Some(3));
}

#[tokio::test]
async fn trouble_the_stub_logs_is_reported_to_the_host_by_kind() {
    let (tx, mut rx) = mpsc::channel(16);
    let table = StandIns::open_with_capacity(None, 1);
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&calls);
    let fake = Arc::new(Fake::new(move |q: ResolveQuery| {
        let n = seen.fetch_add(1, Ordering::SeqCst);
        std::future::ready(match (n, q.name.as_str()) {
            (0, _) => Err(ResolveError::NoAnswer),
            _ => Ok(stand_in()),
        })
    }));
    let stub = Stub::reporting(
        Arc::clone(&fake),
        table.clone(),
        Limits::default(),
        Some(tx),
    );
    // A failed lookup, twice: one report at once, the second waits for its window.
    assert_eq!(ask_stub(&stub, "a.example.com", TYPE_A).await.rcode(), 2);
    let first = ask_stub(&stub, "a.example.com", TYPE_A).await;
    assert_eq!(first.rcode(), 0);
    // Every stand-in in use: no address left.
    let _held = table.hold(first.answers[0].ip()).unwrap();
    assert_eq!(ask_stub(&stub, "b.example.com", TYPE_A).await.rcode(), 2);
    assert_eq!(ask_stub(&stub, "c.example.com", TYPE_A).await.rcode(), 2);
    let mut got = Vec::new();
    // flake-ok: `State::trouble` queues a report with `try_send` before the stub answers, and each
    // `ask_stub` above returned that answer, so the queue already holds every report.
    while let Ok(msg) = rx.try_recv() {
        got.push(msg);
    }
    assert_eq!(
        got,
        [
            AgentMessage::stub_dns(StubDnsTrouble::LookupFailed, 1),
            AgentMessage::stub_dns(StubDnsTrouble::NoStandInAddress, 1),
        ]
    );
}

#[tokio::test]
async fn a_stub_whose_report_queue_is_full_still_answers() {
    let (tx, _rx) = mpsc::channel(1);
    tx.try_send(AgentMessage::hello()).unwrap();
    let fake = Arc::new(Fake::new(|_| {
        std::future::ready(Err(ResolveError::NoAnswer))
    }));
    let stub = Stub::reporting(fake, StandIns::in_memory(), Limits::default(), Some(tx));
    assert_eq!(ask_stub(&stub, "a.example.com", TYPE_A).await.rcode(), 2);
}
