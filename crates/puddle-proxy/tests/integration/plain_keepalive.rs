// SPDX-License-Identifier: GPL-3.0-or-later
//! Plain HTTP on a kept-alive guest connection: every request is decided, judged and recorded on
//! its own, a request for the same host reuses the upstream connection, and nothing a request
//! leaves behind (a pipelined request, a body the server did not wait for, a head cut in pieces)
//! is read as more than it is.
#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "helpers outside #[test] functions fail the test by panicking"
)]

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use puddle_proxy::ProxyConfig;
use puddle_proxy::testing::StaticResolver;
use puddle_types::ConnectionDecision;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

use super::route::{LOCAL, Rig, header_value, host, read_all, response_head};

/// A request as a [`keep_alive_server`] saw it: `METHOD path` and the body.
type Request = (String, Vec<u8>);

/// What a [`keep_alive_server`] saw: each request as `METHOD path` with its body, and how many
/// connections it accepted.
#[derive(Clone, Default)]
struct Seen {
    requests: Arc<Mutex<Vec<Request>>>,
    accepts: Arc<AtomicUsize>,
}

impl Seen {
    fn requests(&self) -> Vec<Request> {
        self.requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn lines(&self) -> Vec<String> {
        self.requests().into_iter().map(|(line, _)| line).collect()
    }

    fn accepts(&self) -> usize {
        self.accepts.load(Ordering::SeqCst)
    }
}

/// An HTTP/1.1 server that keeps its connections open. Each request's answer depends on its
/// path: `/early` is a `413` sent before the body is read (then the server closes), `/204` and
/// `/304` have no body, `/chunked` is chunked, `/close` runs to the end of the connection, `/drop`
/// closes with no answer, `/silent` never answers, `/short` sends 3 of the 10 bytes it announces
/// and closes, `/extra` is followed by a second answer nobody asked for, a `HEAD` gets a length
/// and no body, and anything else gets its own path back as the body.
async fn keep_alive_server() -> (SocketAddr, Seen) {
    let listener = TcpListener::bind((LOCAL, 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Seen::default();
    let shared = seen.clone();
    tokio::spawn(async move {
        while let Ok((conn, _)) = listener.accept().await {
            shared.accepts.fetch_add(1, Ordering::SeqCst);
            let seen = shared.clone();
            tokio::spawn(async move {
                let mut reader = BufReader::new(conn);
                while let Some(close) = serve_one(&mut reader, &seen).await {
                    if close {
                        let _ = reader.get_mut().shutdown().await;
                        return;
                    }
                }
            });
        }
    });
    (addr, seen)
}

/// Serves one request; `None` at the end of the connection, `Some(true)` when the server closes.
async fn serve_one(reader: &mut BufReader<tokio::net::TcpStream>, seen: &Seen) -> Option<bool> {
    let mut line = String::new();
    if reader.read_line(&mut line).await.ok()? == 0 {
        return None;
    }
    let mut parts = line.split(' ');
    let (method, path) = (parts.next()?.to_owned(), parts.next()?.to_owned());
    let (mut length, mut chunked) = (0, false);
    loop {
        let mut header = String::new();
        reader.read_line(&mut header).await.ok()?;
        let lower = header.to_ascii_lowercase();
        if let Some(value) = lower.strip_prefix("content-length:") {
            length = value.trim().parse().ok()?;
        }
        chunked |= lower.starts_with("transfer-encoding:") && lower.contains("chunked");
        if header == "\r\n" || header.is_empty() {
            break;
        }
    }
    let label = format!("{method} {path}");
    match path.as_str() {
        "/drop" => return Some(true),
        "/silent" => {
            tokio::time::sleep(Duration::from_secs(10)).await;
            return Some(true);
        }
        _ => {}
    }
    if path == "/early" {
        seen.requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((label, Vec::new()));
        let answer =
            "HTTP/1.1 413 Payload Too Large\r\ncontent-length: 0\r\nconnection: close\r\n\r\n";
        reader.get_mut().write_all(answer.as_bytes()).await.ok()?;
        return Some(true);
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).await.ok()?;
    if chunked {
        loop {
            let mut size = String::new();
            reader.read_line(&mut size).await.ok()?;
            let size = usize::from_str_radix(size.trim(), 16).ok()?;
            let mut piece = vec![0; size + 2];
            reader.read_exact(&mut piece).await.ok()?;
            if size == 0 {
                break;
            }
            body.extend_from_slice(&piece[..size]);
        }
    }
    seen.requests
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push((label, body));
    let (answer, close) = match (method.as_str(), path.as_str()) {
        ("HEAD", _) => (
            "HTTP/1.1 200 OK\r\ncontent-length: 4\r\n\r\n".to_owned(),
            false,
        ),
        (_, "/204") => ("HTTP/1.1 204 No Content\r\n\r\n".to_owned(), false),
        (_, "/304") => (
            "HTTP/1.1 304 Not Modified\r\ncontent-length: 4\r\n\r\n".to_owned(),
            false,
        ),
        (_, "/chunked") => (
            "HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n3\r\nabc\r\n0\r\n\r\n".to_owned(),
            false,
        ),
        (_, "/close") => ("HTTP/1.1 200 OK\r\n\r\nuntil-close".to_owned(), true),
        (_, "/short") => ("HTTP/1.1 200 OK\r\ncontent-length: 10\r\n\r\nabc".to_owned(), true),
        (_, "/extra") => (
            "HTTP/1.1 200 OK\r\ncontent-length: 6\r\n\r\n/extraHTTP/1.1 200 OK\r\ncontent-length: 6\r\n\r\npoison"
                .to_owned(),
            false,
        ),
        _ => (
            format!(
                "HTTP/1.1 200 OK\r\ncontent-length: {}\r\n\r\n{path}",
                path.len()
            ),
            false,
        ),
    };
    reader.get_mut().write_all(answer.as_bytes()).await.ok()?;
    Some(close)
}

/// One response as the guest reads it: status, header lines and the body its framing gives.
struct Answer {
    code: u16,
    headers: Vec<String>,
    body: Vec<u8>,
}

impl Answer {
    fn closes(&self) -> bool {
        header_value(&self.headers, "connection").is_some_and(|v| v.eq_ignore_ascii_case("close"))
    }
}

async fn answer<R: AsyncRead + Unpin>(reader: &mut BufReader<R>, method: &str) -> Answer {
    let (code, headers) = response_head(reader).await.unwrap();
    let mut body = Vec::new();
    let no_body = method == "HEAD" || code == 204 || code == 304 || (100..200).contains(&code);
    if no_body {
    } else if let Some(length) = header_value(&headers, "content-length") {
        body = vec![0; length.parse().unwrap()];
        reader.read_exact(&mut body).await.unwrap();
    } else if header_value(&headers, "transfer-encoding").is_some() {
        loop {
            let mut size = String::new();
            reader.read_line(&mut size).await.unwrap();
            let size = usize::from_str_radix(size.trim(), 16).unwrap();
            let mut piece = vec![0; size + 2];
            reader.read_exact(&mut piece).await.unwrap();
            if size == 0 {
                break;
            }
            body.extend_from_slice(&piece[..size]);
        }
    } else {
        reader.read_to_end(&mut body).await.unwrap();
    }
    Answer {
        code,
        headers,
        body,
    }
}

/// A rig that knows `web.test`, `other.test` and `evil.test` (all on loopback) and allows the
/// first two.
fn rig(config: &ProxyConfig) -> Rig {
    let rig = Rig::with_resolver(
        config,
        StaticResolver::new()
            .with("web.test", &[LOCAL])
            .with("other.test", &[LOCAL])
            .with("evil.test", &[LOCAL]),
    );
    rig.policy.allow(&host("web.test"));
    rig.policy.allow(&host("other.test"));
    rig
}

fn get(host: &str, server: SocketAddr, path: &str) -> String {
    format!(
        "GET http://{host}:{}{path} HTTP/1.1\r\nHost: {host}\r\n\r\n",
        server.port()
    )
}

/// Reads to the end within 5 s: the proxy ended the connection.
async fn ended<R: AsyncRead + Unpin>(reader: &mut R) -> Vec<u8> {
    tokio::time::timeout(Duration::from_secs(5), read_all(reader))
        .await
        .expect("the proxy kept the connection open")
        .unwrap_or_default()
}

#[tokio::test]
async fn requests_on_a_kept_connection_share_one_upstream_connection_and_are_each_recorded() {
    let (web, seen) = keep_alive_server().await;
    let rig = rig(&ProxyConfig::default());
    let mut guest = rig.guest().await;
    let mut reader = BufReader::new(guest.stream().await);
    let mut down = 0;
    for path in ["/1", "/2", "/3"] {
        reader
            .get_mut()
            .write_all(get("web.test", web, path).as_bytes())
            .await
            .unwrap();
        let got = answer(&mut reader, "GET").await;
        assert_eq!((got.code, got.body.as_slice()), (200, path.as_bytes()));
        assert!(!got.closes(), "the connection stays open");
    }
    assert_eq!(seen.accepts(), 1, "one upstream connection for all three");
    let events = rig.events(3).await;
    let paths: Vec<_> = events
        .iter()
        .map(|e| e.http.as_ref().unwrap().path().to_owned())
        .collect();
    assert_eq!(paths, ["/1", "/2", "/3"]);
    for event in &events {
        assert_eq!(event.decision, ConnectionDecision::Allow);
        assert_eq!(event.resolved_ip, Some(LOCAL));
        assert!(event.bytes_up > 0 && event.bytes_down > 0, "{event:?}");
        down += event.bytes_down;
    }
    drop(reader);
    assert!(down > 0);
}

#[tokio::test]
async fn responses_of_every_framing_leave_the_connection_usable() {
    let (web, seen) = keep_alive_server().await;
    let rig = rig(&ProxyConfig::default());
    let mut guest = rig.guest().await;
    let mut reader = BufReader::new(guest.stream().await);
    let port = web.port();
    for (method, path, code, body) in [
        ("HEAD", "/head", 200, &b""[..]),
        ("GET", "/204", 204, b""),
        ("GET", "/304", 304, b""),
        ("GET", "/chunked", 200, b"abc"),
        ("GET", "/close", 200, b"until-close"),
        ("GET", "/after", 200, b"/after"),
    ] {
        let request =
            format!("{method} http://web.test:{port}{path} HTTP/1.1\r\nHost: web.test\r\n\r\n");
        reader
            .get_mut()
            .write_all(request.as_bytes())
            .await
            .unwrap();
        let got = answer(&mut reader, method).await;
        assert_eq!(
            (got.code, got.body.as_slice()),
            (code, body),
            "{method} {path}"
        );
        assert!(
            !got.closes(),
            "{method} {path} ended the guest's connection"
        );
        if method == "HEAD" {
            assert_eq!(
                header_value(&got.headers, "content-length").as_deref(),
                Some("4"),
                "a HEAD answer keeps the length it describes"
            );
        }
    }
    assert_eq!(
        seen.accepts(),
        2,
        "the server closed after /close, so /after went on a new upstream connection"
    );
    assert_eq!(rig.events(6).await.len(), 6);
}

/// The keep-alive host switch: a kept connection asks for `web.test`, then `other.test`, then
/// `web.test` again, then `evil.test`. Each request is decided for its own host and goes out on a
/// connection to that host; `evil.test` has no rule, so it becomes a pending request and a `403`,
/// nothing connects to it, and the connection ends.
#[tokio::test]
async fn hostile_hg09_a_host_switch_on_a_kept_connection_is_decided_and_connected_anew() {
    let (web, web_seen) = keep_alive_server().await;
    let (other, other_seen) = keep_alive_server().await;
    let trap = TcpListener::bind((LOCAL, 0)).await.unwrap();
    let evil = trap.local_addr().unwrap();
    let rig = rig(&ProxyConfig::default());
    let mut guest = rig.guest().await;
    let mut reader = BufReader::new(guest.stream().await);
    for (host, server, path) in [
        ("web.test", web, "/1"),
        ("other.test", other, "/2"),
        ("web.test", web, "/3"),
    ] {
        reader
            .get_mut()
            .write_all(get(host, server, path).as_bytes())
            .await
            .unwrap();
        let got = answer(&mut reader, "GET").await;
        assert_eq!(
            (got.code, got.body.as_slice()),
            (200, path.as_bytes()),
            "{host}"
        );
    }
    reader
        .get_mut()
        .write_all(get("evil.test", evil, "/4").as_bytes())
        .await
        .unwrap();
    let (code, headers) = response_head(&mut reader).await.unwrap();
    assert_eq!(code, 403);
    assert_eq!(
        header_value(&headers, "x-puddle-decision").as_deref(),
        Some("pending")
    );
    ended(&mut reader).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(300), trap.accept())
            .await
            .is_err(),
        "evil.test was connected to"
    );
    assert_eq!(web_seen.lines(), ["GET /1", "GET /3"]);
    assert_eq!(other_seen.lines(), ["GET /2"]);
    assert_eq!(
        (web_seen.accepts(), other_seen.accepts()),
        (2, 1),
        "a switch of host never reuses the other host's connection"
    );
    let hosts: Vec<_> = rig
        .events(4)
        .await
        .iter()
        .map(|e| (e.host.to_string(), e.decision))
        .collect();
    assert_eq!(
        hosts,
        [
            ("web.test".to_owned(), ConnectionDecision::Allow),
            ("other.test".to_owned(), ConnectionDecision::Allow),
            ("web.test".to_owned(), ConnectionDecision::Allow),
            ("evil.test".to_owned(), ConnectionDecision::Pending),
        ]
    );
    assert_eq!(rig.policy.decisions(), 4);
}

/// A rule that changes between two requests of a kept connection applies to the second one: the
/// upstream connection that is already open is not a way past it.
#[tokio::test]
async fn hostile_a_rule_changed_between_two_requests_decides_the_second() {
    let (web, seen) = keep_alive_server().await;
    let rig = rig(&ProxyConfig::default());
    let mut guest = rig.guest().await;
    let mut reader = BufReader::new(guest.stream().await);
    reader
        .get_mut()
        .write_all(get("web.test", web, "/1").as_bytes())
        .await
        .unwrap();
    assert_eq!(answer(&mut reader, "GET").await.code, 200);
    rig.policy.deny_host(&host("web.test"));
    reader
        .get_mut()
        .write_all(get("web.test", web, "/2").as_bytes())
        .await
        .unwrap();
    let (code, headers) = response_head(&mut reader).await.unwrap();
    assert_eq!(code, 403);
    assert_eq!(
        header_value(&headers, "x-puddle-decision").as_deref(),
        Some("deny")
    );
    ended(&mut reader).await;
    assert_eq!(
        seen.lines(),
        ["GET /1"],
        "the denied request never went upstream"
    );
    let events = rig.events(2).await;
    assert_eq!(events[1].decision, ConnectionDecision::Deny);
}

/// Requests sent in one go are read one at a time, each after the answer to the one before:
/// each is decided on its own, and once one is refused the rest are never read.
#[tokio::test]
async fn hostile_pipelined_requests_are_each_decided_in_order() {
    let (web, seen) = keep_alive_server().await;
    let rig = rig(&ProxyConfig::default());
    let mut guest = rig.guest().await;
    let mut reader = BufReader::new(guest.stream().await);
    let pipelined = [
        get("web.test", web, "/a"),
        get("web.test", web, "/b"),
        get("evil.test", web, "/c"),
        get("web.test", web, "/d"),
    ]
    .concat();
    reader
        .get_mut()
        .write_all(pipelined.as_bytes())
        .await
        .unwrap();
    for path in ["/a", "/b"] {
        let got = answer(&mut reader, "GET").await;
        assert_eq!((got.code, got.body.as_slice()), (200, path.as_bytes()));
    }
    let (code, _) = response_head(&mut reader).await.unwrap();
    assert_eq!(code, 403);
    ended(&mut reader).await;
    assert_eq!(seen.lines(), ["GET /a", "GET /b"]);
    assert_eq!(seen.accepts(), 1);
    assert_eq!(
        rig.policy.decisions(),
        3,
        "the request after the refused one was never read"
    );
    assert_eq!(rig.events(3).await.len(), 3);
}

/// The server answers before it has the body (a `413` on the head alone). The body was not read
/// to its end, so the rest of it is still on the guest connection: the proxy closes the connection
/// after the answer instead of reading on, and a request hidden in that rest is never read.
#[tokio::test]
async fn hostile_a_body_left_unread_ends_the_connection_and_is_never_read_as_a_request() {
    let (web, seen) = keep_alive_server().await;
    let trap = TcpListener::bind((LOCAL, 0)).await.unwrap();
    let hidden = get("evil.test", trap.local_addr().unwrap(), "/hidden");
    let rest = format!("{}{hidden}", "x".repeat(16));
    let rig = rig(&ProxyConfig::default());
    let mut guest = rig.guest().await;
    let mut reader = BufReader::new(guest.stream().await);
    let head = format!(
        "POST http://web.test:{}/early HTTP/1.1\r\nHost: web.test\r\nContent-Length: {}\r\n\r\n{}",
        web.port(),
        16 + rest.len(),
        "y".repeat(16),
    );
    reader.get_mut().write_all(head.as_bytes()).await.unwrap();
    let got = answer(&mut reader, "POST").await;
    assert_eq!(got.code, 413);
    // The rest of the body arrives now, with a whole request at its end.
    let _ = reader.get_mut().write_all(rest.as_bytes()).await;
    let after = ended(&mut reader).await;
    assert_eq!(after, b"", "nothing was answered after the 413");
    assert!(
        tokio::time::timeout(Duration::from_millis(300), trap.accept())
            .await
            .is_err(),
        "the hidden request was forwarded"
    );
    assert_eq!(seen.lines(), ["POST /early"]);
    assert_eq!(
        (rig.policy.decisions(), rig.policy.pending().len()),
        (1, 0),
        "the hidden request was never decided"
    );
}

/// A body that holds the text of a request is a body: it goes upstream as one, by its
/// `Content-Length`, and the request that follows it on the connection is the next one.
#[tokio::test]
async fn hostile_hg16_a_body_is_never_read_as_a_second_request() {
    let (web, seen) = keep_alive_server().await;
    let rig = rig(&ProxyConfig::default());
    let mut guest = rig.guest().await;
    let mut reader = BufReader::new(guest.stream().await);
    let fake = "GET http://evil.test/ HTTP/1.1\r\nHost: evil.test\r\n\r\n";
    let chunked = format!("{:x}\r\n{fake}\r\n0\r\n\r\n", fake.len());
    let requests = [
        format!(
            "POST http://web.test:{}/len HTTP/1.1\r\nHost: web.test\r\nContent-Length: {}\r\n\r\n{fake}",
            web.port(),
            fake.len()
        ),
        format!(
            "POST http://web.test:{}/chunks HTTP/1.1\r\nHost: web.test\r\nTransfer-Encoding: chunked\r\n\r\n{chunked}",
            web.port()
        ),
        get("web.test", web, "/next"),
    ]
    .concat();
    reader
        .get_mut()
        .write_all(requests.as_bytes())
        .await
        .unwrap();
    for (method, body) in [
        ("POST", &b"/len"[..]),
        ("POST", b"/chunks"),
        ("GET", b"/next"),
    ] {
        let got = answer(&mut reader, method).await;
        assert_eq!((got.code, got.body.as_slice()), (200, body));
    }
    let requests = seen.requests();
    assert_eq!(
        requests,
        [
            ("POST /len".to_owned(), fake.as_bytes().to_vec()),
            ("POST /chunks".to_owned(), fake.as_bytes().to_vec()),
            ("GET /next".to_owned(), Vec::new()),
        ]
    );
    assert_eq!(rig.policy.pending().len(), 0, "evil.test was never decided");
}

/// A second request whose framing two readers would read differently is refused with a `400`
/// before anything of it goes upstream, and the connection ends there.
#[tokio::test]
async fn hostile_hg16_ambiguous_framing_on_a_kept_connection_is_refused_before_it_goes_upstream() {
    let (web, seen) = keep_alive_server().await;
    let rig = rig(&ProxyConfig::default());
    let framings = [
        "Content-Length: 4\r\nTransfer-Encoding: chunked",
        "Content-Length: 4\r\nContent-Length: 5",
        "Transfer-Encoding: chunked, gzip",
        "Transfer-Encoding: xchunked",
        "Transfer-Encoding : chunked",
    ];
    for framing in framings {
        let mut guest = rig.guest().await;
        let mut reader = BufReader::new(guest.stream().await);
        let second = format!(
            "POST http://web.test:{}/smuggled HTTP/1.1\r\nHost: web.test\r\n{framing}\r\n\r\n0\r\n\r\n{}",
            web.port(),
            get("evil.test", web, "/hidden")
        );
        let both = format!("{}{second}", get("web.test", web, "/ok"));
        reader.get_mut().write_all(both.as_bytes()).await.unwrap();
        assert_eq!(answer(&mut reader, "GET").await.code, 200, "{framing}");
        let (code, _) = response_head(&mut reader).await.unwrap();
        assert_eq!(code, 400, "{framing}");
        ended(&mut reader).await;
    }
    assert_eq!(seen.lines(), vec!["GET /ok"; framings.len()]);
    assert_eq!(rig.policy.pending().len(), 0);
}

/// The next head may come in pieces: it is read whole across the reads and decided once.
#[tokio::test]
async fn a_head_split_across_reads_on_a_kept_connection_is_read_whole() {
    let (web, seen) = keep_alive_server().await;
    let rig = rig(&ProxyConfig::default());
    let mut guest = rig.guest().await;
    let mut reader = BufReader::new(guest.stream().await);
    reader
        .get_mut()
        .write_all(get("web.test", web, "/first").as_bytes())
        .await
        .unwrap();
    assert_eq!(answer(&mut reader, "GET").await.code, 200);
    let second = get("web.test", web, "/split");
    for piece in [&second[..2], &second[2..15], &second[15..40], &second[40..]] {
        reader.get_mut().write_all(piece.as_bytes()).await.unwrap();
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    let got = answer(&mut reader, "GET").await;
    assert_eq!((got.code, got.body.as_slice()), (200, &b"/split"[..]));
    assert_eq!(seen.lines(), ["GET /first", "GET /split"]);
    assert_eq!(rig.policy.decisions(), 2);
}

/// A second head that stops halfway is cut off by the head timeout, as a first one is.
#[tokio::test]
async fn hostile_a_second_head_that_stalls_is_cut_off() {
    let (web, _) = keep_alive_server().await;
    let rig = rig(&ProxyConfig::default().with_head_timeout(Duration::from_millis(300)));
    let mut guest = rig.guest().await;
    let mut reader = BufReader::new(guest.stream().await);
    reader
        .get_mut()
        .write_all(get("web.test", web, "/first").as_bytes())
        .await
        .unwrap();
    assert_eq!(answer(&mut reader, "GET").await.code, 200);
    let started = Instant::now();
    reader
        .get_mut()
        .write_all(b"GET http://web.test/ HTTP/1.1\r\nX-Slow: ")
        .await
        .unwrap();
    let (code, _) = response_head(&mut reader).await.unwrap();
    assert_eq!(code, 408);
    assert!(started.elapsed() >= Duration::from_millis(300));
    ended(&mut reader).await;
    assert_eq!(rig.policy.decisions(), 1);
}

/// A kept connection the guest leaves idle is closed after the keep-alive time.
#[tokio::test]
async fn an_idle_kept_connection_is_closed_after_the_keepalive_time() {
    let (web, _) = keep_alive_server().await;
    let keepalive = Duration::from_millis(200);
    let config = ProxyConfig::default().with_terminate_timeouts(
        Duration::from_secs(10),
        keepalive,
        Duration::from_secs(300),
        Duration::from_secs(120),
    );
    let rig = rig(&config);
    let mut guest = rig.guest().await;
    let mut reader = BufReader::new(guest.stream().await);
    reader
        .get_mut()
        .write_all(get("web.test", web, "/only").as_bytes())
        .await
        .unwrap();
    assert_eq!(answer(&mut reader, "GET").await.code, 200);
    let started = Instant::now();
    assert_eq!(ended(&mut reader).await, b"");
    assert!(
        started.elapsed() >= keepalive / 2,
        "{:?}",
        started.elapsed()
    );
    assert_eq!(rig.events(1).await.len(), 1);
}

/// The last request a connection may carry gets `connection: close` on its answer, and the
/// connection ends there: a request sent after it is never read.
#[tokio::test]
async fn the_request_limit_ends_the_connection_with_the_last_answer() {
    let (web, seen) = keep_alive_server().await;
    let rig = rig(&ProxyConfig::default().with_max_requests_per_connection(2));
    let mut guest = rig.guest().await;
    let mut reader = BufReader::new(guest.stream().await);
    let three = [
        get("web.test", web, "/1"),
        get("web.test", web, "/2"),
        get("web.test", web, "/3"),
    ]
    .concat();
    reader.get_mut().write_all(three.as_bytes()).await.unwrap();
    let first = answer(&mut reader, "GET").await;
    assert!(!first.closes());
    let second = answer(&mut reader, "GET").await;
    assert_eq!(second.body, b"/2");
    assert!(
        second.closes(),
        "the last answer says the connection closes"
    );
    assert_eq!(ended(&mut reader).await, b"");
    assert_eq!(seen.lines(), ["GET /1", "GET /2"]);
    assert_eq!(rig.policy.decisions(), 2);
}

/// An HTTP/1.0 request, or one that asks to close, gets its answer and then the end of the
/// connection; `Expect: 100-continue` is answered by the proxy before the body is sent.
#[tokio::test]
async fn a_client_that_asks_to_close_or_to_continue_gets_what_it_asked_for() {
    let (web, seen) = keep_alive_server().await;
    let rig = rig(&ProxyConfig::default());
    let port = web.port();
    for request in [
        format!("GET http://web.test:{port}/old HTTP/1.0\r\n\r\n"),
        format!(
            "GET http://web.test:{port}/bye HTTP/1.1\r\nHost: web.test\r\nConnection: close\r\n\r\n"
        ),
    ] {
        let mut guest = rig.guest().await;
        let mut reader = BufReader::new(guest.stream().await);
        reader
            .get_mut()
            .write_all(request.as_bytes())
            .await
            .unwrap();
        let (code, headers) = response_head(&mut reader).await.unwrap();
        assert_eq!(code, 200, "{request}");
        assert_eq!(
            header_value(&headers, "connection").as_deref(),
            Some("close")
        );
        assert!(
            !ended(&mut reader).await.is_empty(),
            "the body came before the end"
        );
    }
    let mut guest = rig.guest().await;
    let mut reader = BufReader::new(guest.stream().await);
    let head = format!(
        "PUT http://web.test:{port}/up HTTP/1.1\r\nHost: web.test\r\nExpect: 100-continue\r\nContent-Length: 4\r\n\r\n"
    );
    reader.get_mut().write_all(head.as_bytes()).await.unwrap();
    assert_eq!(response_head(&mut reader).await.unwrap().0, 100);
    reader.get_mut().write_all(b"data").await.unwrap();
    assert_eq!(answer(&mut reader, "PUT").await.code, 200);
    assert_eq!(
        seen.requests().last().cloned(),
        Some(("PUT /up".to_owned(), b"data".to_vec()))
    );
}

/// A server that sends a second answer nobody asked for cannot have it taken for the answer to
/// the guest's next request: the upstream connection is not used again, and the next request
/// gets its own answer on a new one.
#[tokio::test]
async fn hostile_an_answer_the_server_sends_unasked_never_answers_the_next_request() {
    let (web, seen) = keep_alive_server().await;
    let rig = rig(&ProxyConfig::default());
    let mut guest = rig.guest().await;
    let mut reader = BufReader::new(guest.stream().await);
    for path in ["/extra", "/next"] {
        reader
            .get_mut()
            .write_all(get("web.test", web, path).as_bytes())
            .await
            .unwrap();
        let got = answer(&mut reader, "GET").await;
        assert_eq!((got.code, got.body.as_slice()), (200, path.as_bytes()));
    }
    assert_eq!(seen.accepts(), 2, "the poisoned connection was used again");
}

/// What the upstream does wrong reaches the guest as a refusal that says so, or as a cut-off
/// answer, and ends the connection: a server that closes without an answer is a `502`, one that
/// does not answer in time a `504`, and an answer shorter than its length ends without a clean
/// close.
#[tokio::test]
async fn an_upstream_that_fails_ends_the_connection_with_what_happened() {
    let (web, _) = keep_alive_server().await;
    let config = ProxyConfig::default().with_terminate_timeouts(
        Duration::from_secs(10),
        Duration::from_secs(60),
        Duration::from_millis(300),
        Duration::from_secs(120),
    );
    let rig = rig(&config);
    for (path, code) in [("/drop", 502), ("/silent", 504)] {
        let mut guest = rig.guest().await;
        let mut reader = BufReader::new(guest.stream().await);
        reader
            .get_mut()
            .write_all(get("web.test", web, path).as_bytes())
            .await
            .unwrap();
        let (got, _) = response_head(&mut reader).await.unwrap();
        assert_eq!(got, code, "{path}");
        ended(&mut reader).await;
    }
    let mut guest = rig.guest().await;
    let mut reader = BufReader::new(guest.stream().await);
    reader
        .get_mut()
        .write_all(get("web.test", web, "/short").as_bytes())
        .await
        .unwrap();
    let (code, headers) = response_head(&mut reader).await.unwrap();
    assert_eq!(code, 200);
    assert_eq!(
        header_value(&headers, "content-length").as_deref(),
        Some("10")
    );
    let mut body = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(5), reader.read_to_end(&mut body))
        .await
        .expect("the cut-off answer kept the connection open");
    assert!(body.len() < 10, "{body:?}");
    assert_eq!(rig.events(3).await.len(), 3);
}

/// What the guest sends wrong on a kept connection is refused and ends it: a body whose chunk
/// framing breaks halfway (`400`, and the server never sees a complete request), an expectation
/// other than `100-continue` (`417`) and a `CONNECT` (`400`).
#[tokio::test]
async fn a_broken_second_request_is_refused_and_ends_the_connection() {
    let (web, seen) = keep_alive_server().await;
    let rig = rig(&ProxyConfig::default());
    let port = web.port();
    for (second, code) in [
        (
            format!(
                "POST http://web.test:{port}/broken HTTP/1.1\r\nHost: web.test\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\nzz\r\n"
            ),
            400,
        ),
        (
            format!(
                "POST http://web.test:{port}/x HTTP/1.1\r\nHost: web.test\r\nExpect: magic\r\nContent-Length: 1\r\n\r\nx"
            ),
            417,
        ),
        (format!("CONNECT web.test:{port} HTTP/1.1\r\n\r\n"), 400),
    ] {
        let mut guest = rig.guest().await;
        let mut reader = BufReader::new(guest.stream().await);
        reader
            .get_mut()
            .write_all(get("web.test", web, "/ok").as_bytes())
            .await
            .unwrap();
        assert_eq!(answer(&mut reader, "GET").await.code, 200);
        reader.get_mut().write_all(second.as_bytes()).await.unwrap();
        let (got, _) = response_head(&mut reader).await.unwrap();
        assert_eq!(got, code, "{second}");
        ended(&mut reader).await;
    }
    assert!(
        seen.requests().iter().all(|(line, _)| line == "GET /ok"),
        "{:?}",
        seen.lines()
    );
}
