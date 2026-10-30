// SPDX-License-Identifier: GPL-3.0-or-later
use std::time::Duration;

use futures_util::StreamExt;
use puddle_test_kit::{eventually, no_event_for};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_yamux::{Control, Session};

use super::*;
use crate::control::{PREAMBLE, PROTOCOL_VERSION};
use crate::kind::StreamKind;
use crate::resolve::{self, RecordType};
use crate::yamux::client_config;

const WAIT: Duration = Duration::from_secs(5);

struct ChanSink(mpsc::UnboundedSender<Event>);

impl EventSink for ChanSink {
    fn emit(&self, event: Event) {
        // The receiver outlives every session in these tests.
        let _ = self.0.send(event);
    }
}

/// Echoes a stream back, first byte included.
struct Echo;

impl StreamHandler for Echo {
    async fn handle(&self, mut stream: GuestStream) {
        let mut buf = Vec::new();
        if stream.read_to_end(&mut buf).await.is_ok() {
            let _ = stream.write_all(&buf).await;
            let _ = stream.shutdown().await;
        }
    }

    async fn resolve(&self, query: ResolveQuery) -> ResolveAnswer {
        if query.name == "slow.example" {
            // A resolver that never answers within the test: the caller's own timeout ends the wait.
            tokio::time::sleep(Duration::from_secs(60)).await;
        }
        ResolveAnswer::NoSuchName {
            ttl: u32::try_from(query.name.len()).unwrap(),
        }
    }
}

struct Harness {
    control: Control,
    server: JoinHandle<Result<(), SessionError>>,
    events: mpsc::UnboundedReceiver<Event>,
    /// Streams the host opened to this fake agent.
    inbound: mpsc::UnboundedReceiver<StreamHandle>,
    _driver: JoinHandle<()>,
}

fn name() -> WorkspaceName {
    WorkspaceName::new("box").unwrap()
}

fn start_with(config: HostConfig) -> Harness {
    start_handler(Arc::new(Echo), config)
}

fn start_handler<H: StreamHandler>(handler: Arc<H>, config: HostConfig) -> Harness {
    let (guest, host) = tokio::io::duplex(1 << 20);
    let (tx, events) = mpsc::unbounded_channel();
    let server = tokio::spawn(serve_session(
        host,
        name(),
        Arc::new(ChanSink(tx)),
        handler,
        config,
    ));
    let mut client = Session::new_client(guest, client_config());
    let control = client.control();
    let (opened, inbound) = mpsc::unbounded_channel();
    let driver = tokio::spawn(async move {
        while let Some(next) = client.next().await {
            if let Ok(stream) = next {
                let _ = opened.send(stream);
            }
        }
    });
    Harness {
        control,
        server,
        events,
        inbound,
        _driver: driver,
    }
}

fn start() -> Harness {
    start_with(HostConfig::default())
}

impl Harness {
    async fn control_stream(&mut self) -> StreamHandle {
        let mut s = self.control.open_stream().await.unwrap();
        s.write_all(PREAMBLE).await.unwrap();
        s
    }

    async fn next_event(&mut self) -> Event {
        tokio::time::timeout(WAIT, self.events.recv())
            .await
            .expect("no event in time")
            .expect("sink closed")
    }

    async fn close(mut self) -> Result<(), SessionError> {
        self.control.close().await;
        tokio::time::timeout(WAIT, self.server)
            .await
            .unwrap()
            .unwrap()
    }
}

async fn send(stream: &mut StreamHandle, msg: &AgentMessage) {
    stream.write_all(&msg.to_line().unwrap()).await.unwrap();
}

#[tokio::test]
async fn a_proxied_stream_reaches_the_handler_with_its_first_byte() {
    let mut h = start();
    let mut s = h.control.open_stream().await.unwrap();
    s.write_all(b"CONNECT example.test:443 HTTP/1.1\r\n\r\n")
        .await
        .unwrap();
    s.shutdown().await.unwrap();
    let mut back = Vec::new();
    s.read_to_end(&mut back).await.unwrap();
    assert_eq!(back, b"CONNECT example.test:443 HTTP/1.1\r\n\r\n");
    h.close().await.unwrap();
}

#[tokio::test]
async fn oom_kills_on_the_control_stream_become_events_for_the_routes_workspace() {
    let mut h = start();
    let mut c = h.control_stream().await;
    send(&mut c, &AgentMessage::hello()).await;
    send(
        &mut c,
        &AgentMessage::oom_kill(Some(2406), Some("tail".into())),
    )
    .await;
    send(&mut c, &AgentMessage::oom_kill(None, None)).await;
    send(
        &mut c,
        &AgentMessage::oom_kill(Some(9), Some("evil\u{1b}[2J".into())),
    )
    .await;
    assert_eq!(h.next_event().await, Event::oom_kill(name(), 2406, "tail"));
    assert_eq!(
        h.next_event().await,
        Event::oom_kill(name(), 0, UNKNOWN_PROCESS)
    );
    let Event::OomKill { process, .. } = h.next_event().await else {
        panic!("wrong event");
    };
    assert_eq!(process, "evil?[2J");
    h.close().await.unwrap();
}

/// What a handler is told about the guest's capture rules.
#[derive(Default)]
struct Told {
    udp: std::sync::Mutex<Vec<UdpRefusal>>,
    tampered: std::sync::Mutex<Vec<bool>>,
    reports: std::sync::Mutex<Vec<AgentReport>>,
}

struct Listening(Arc<Told>);

impl StreamHandler for Listening {
    async fn handle(&self, _stream: GuestStream) {}

    async fn udp_refused(&self, refusal: UdpRefusal) {
        self.0.udp.lock().unwrap().push(refusal);
    }

    async fn capture_tampered(&self, table_missing: bool) {
        self.0.tampered.lock().unwrap().push(table_missing);
    }

    async fn agent_report(&self, report: AgentReport) {
        self.0.reports.lock().unwrap().push(report);
    }
}

#[tokio::test]
async fn capture_reports_reach_the_handler_with_the_guests_text_cleaned() {
    let told = Arc::new(Told::default());
    let mut h = start_handler(
        Arc::new(Listening(Arc::clone(&told))),
        HostConfig::default(),
    );
    let mut c = h.control_stream().await;
    send(&mut c, &AgentMessage::hello()).await;
    send(
        &mut c,
        &AgentMessage::udp_refused("192.0.2.9".into(), 443, 3),
    )
    .await;
    send(
        &mut c,
        &AgentMessage::udp_refused("\u{1b}[2J".repeat(40), 53, 1),
    )
    .await;
    send(&mut c, &AgentMessage::capture_tampered(true)).await;
    send(&mut c, &AgentMessage::capture_tampered(false)).await;
    send(
        &mut c,
        &AgentMessage::capture_unavailable("no nf_tables\u{1b}[2J".repeat(40)),
    )
    .await;
    // The session is still reading: wait until the last report was handled.
    for _ in 0..100 {
        if told.tampered.lock().unwrap().len() == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let udp = told.udp.lock().unwrap().clone();
    assert_eq!(udp.len(), 2);
    assert_eq!(
        udp[0],
        UdpRefusal {
            address: "192.0.2.9".into(),
            port: 443,
            count: 3
        }
    );
    assert!(
        udp[1].address.chars().count() <= MAX_ADDRESS_CHARS && !udp[1].address.contains('\u{1b}'),
        "{:?}",
        udp[1].address
    );
    assert_eq!(*told.tampered.lock().unwrap(), vec![true, false]);
    h.close().await.unwrap();
}

#[tokio::test]
async fn entrypoint_and_stub_dns_reports_reach_the_handler_cleaned_and_unknown_kinds_do_not() {
    let told = Arc::new(Told::default());
    let mut h = start_handler(
        Arc::new(Listening(Arc::clone(&told))),
        HostConfig::default(),
    );
    let mut c = h.control_stream().await;
    send(
        &mut c,
        &AgentMessage::entrypoint_exited(127, "x: not found".into()),
    )
    .await;
    send(
        &mut c,
        &AgentMessage::entrypoint_exited(1, "\u{1b}[2Jboom\u{7}".repeat(100)),
    )
    .await;
    c.write_all(b"{\"type\":\"stub_dns\",\"kind\":\"cosmic_rays\",\"count\":9}\n")
        .await
        .unwrap();
    send(
        &mut c,
        &AgentMessage::stub_dns(StubDnsTrouble::LookupTimedOut, 4),
    )
    .await;
    for _ in 0..100 {
        if told.reports.lock().unwrap().len() == 3 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let reports = told.reports.lock().unwrap().clone();
    assert_eq!(reports.len(), 3, "{reports:?}");
    assert_eq!(
        reports[0],
        AgentReport::EntrypointExited {
            status: 127,
            tail: "x: not found".into()
        }
    );
    let AgentReport::EntrypointExited { tail, .. } = &reports[1] else {
        panic!("{reports:?}");
    };
    assert_eq!(tail.chars().count(), MAX_TAIL_CHARS);
    assert!(tail.starts_with("?[2Jboom?") && !tail.chars().any(char::is_control));
    assert_eq!(
        reports[2],
        AgentReport::StubDns {
            kind: StubDnsTrouble::LookupTimedOut,
            count: 4
        }
    );
    h.close().await.unwrap();
}

#[tokio::test]
async fn a_handler_that_takes_no_reports_still_gets_the_events_after_them() {
    let mut h = start();
    let mut c = h.control_stream().await;
    send(&mut c, &AgentMessage::entrypoint_exited(0, String::new())).await;
    send(
        &mut c,
        &AgentMessage::stub_dns(StubDnsTrouble::UdpDropped, 1),
    )
    .await;
    send(&mut c, &AgentMessage::oom_kill(Some(1), Some("a".into()))).await;
    assert_eq!(h.next_event().await, Event::oom_kill(name(), 1, "a"));
    h.close().await.unwrap();
}

#[test]
fn guest_text_in_logs_is_cut_and_stripped_of_control_characters() {
    let reason = clean(&"no nf_tables\u{1b}[2J".repeat(40), MAX_REASON_CHARS);
    assert_eq!(reason.chars().count(), MAX_REASON_CHARS);
    assert!(!reason.contains('\u{1b}'));
    assert!(reason.starts_with("no nf_tables?[2J"));
}

#[tokio::test]
async fn a_handler_that_does_not_care_about_capture_reports_loses_nothing_else() {
    let mut h = start();
    let mut c = h.control_stream().await;
    send(
        &mut c,
        &AgentMessage::udp_refused("192.0.2.9".into(), 443, 1),
    )
    .await;
    send(&mut c, &AgentMessage::capture_tampered(true)).await;
    send(&mut c, &AgentMessage::oom_kill(Some(1), Some("a".into()))).await;
    assert_eq!(h.next_event().await, Event::oom_kill(name(), 1, "a"));
    h.close().await.unwrap();
}

#[tokio::test]
async fn unknown_and_invalid_messages_are_skipped_and_later_ones_still_count() {
    let mut h = start();
    let mut c = h.control_stream().await;
    c.write_all(b"{\"type\":\"future_thing\"}\nnot json\n{\"type\":\"hello\",\"agent_version\":\"\\u0007x\",\"protocol\":9}\n")
        .await
        .unwrap();
    send(&mut c, &AgentMessage::oom_kill(Some(1), Some("a".into()))).await;
    assert_eq!(h.next_event().await, Event::oom_kill(name(), 1, "a"));
    h.close().await.unwrap();
}

#[tokio::test]
async fn a_guest_agent_on_another_protocol_is_logged_and_kept() {
    Logs::capture();
    let mut h = start();
    let mut c = h.control_stream().await;
    let other = AgentMessage::Hello {
        agent_version: "0.0.0".to_owned(),
        protocol: PROTOCOL_VERSION + 1,
    };
    send(&mut c, &other).await;
    send(&mut c, &AgentMessage::oom_kill(Some(1), Some("a".into()))).await;
    assert_eq!(h.next_event().await, Event::oom_kill(name(), 1, "a"));
    h.close().await.unwrap();
    let text = Logs::text();
    assert!(
        text.contains("WARN") && text.contains("another control protocol"),
        "{text}"
    );
    assert!(
        text.contains(&format!("agent_protocol={}", PROTOCOL_VERSION + 1)),
        "{text}"
    );
    assert!(
        text.contains(&format!("host_protocol={PROTOCOL_VERSION}")),
        "{text}"
    );
}

#[tokio::test]
async fn a_second_control_stream_on_a_session_is_refused_while_the_first_is_open() {
    let mut h = start();
    let mut first = h.control_stream().await;
    send(&mut first, &AgentMessage::hello()).await;
    // Let the host register the first stream before the second arrives.
    send(&mut first, &AgentMessage::oom_kill(Some(1), None)).await;
    assert_eq!(h.next_event().await, Event::oom_kill(name(), 1, "unknown"));

    let mut second = h.control_stream().await;
    send(&mut second, &AgentMessage::oom_kill(Some(2), None)).await;
    let mut buf = [0u8; 1];
    // The host dropped the second stream without a shutdown: a reset, not data.
    assert!(second.read(&mut buf).await.is_err());

    send(&mut first, &AgentMessage::oom_kill(Some(3), None)).await;
    assert_eq!(h.next_event().await, Event::oom_kill(name(), 3, "unknown"));

    // After the first one closes, a new control stream is accepted (agent reconnect).
    first.shutdown().await.unwrap();
    drop(first);
    let mut third = h.control_stream().await;
    let mut accepted = false;
    for pid in 4..200 {
        if third
            .write_all(&AgentMessage::oom_kill(Some(pid), None).to_line().unwrap())
            .await
            .is_err()
        {
            third = h.control_stream().await;
            continue;
        }
        if let Ok(Some(Event::OomKill { pid: got, .. })) =
            tokio::time::timeout(Duration::from_millis(50), h.events.recv()).await
        {
            assert!(got >= 4);
            accepted = true;
            break;
        }
    }
    assert!(
        accepted,
        "a control stream after the first closed was refused"
    );
    h.close().await.unwrap();
}

#[tokio::test]
async fn a_control_stream_with_a_bad_preamble_is_dropped() {
    let mut h = start();
    let mut s = h.control.open_stream().await.unwrap();
    s.write_all(b"\0puddle-kontrol/1\n").await.unwrap();
    send(&mut s, &AgentMessage::oom_kill(Some(1), None)).await;
    let mut buf = [0u8; 1];
    assert!(s.read(&mut buf).await.is_err());
    // Closing joins the session, which drops its end of the channel: no event is read at once, and a
    // late one would still show inside the window.
    let mut events = std::mem::replace(&mut h.events, mpsc::unbounded_channel().1);
    h.close().await.unwrap();
    no_event_for(
        "an event for a stream with a bad preamble",
        Duration::from_millis(200),
        &mut events,
    )
    .await;
}

#[tokio::test]
async fn reserved_and_malformed_stream_kinds_are_closed() {
    let mut h = start();
    for preamble in [
        StreamKind::Connect.preamble(),
        StreamKind::SshAgent.preamble(),
        b"\0puddle-control/2\n",
        b"\0no newline in sixty-four bytes .................................................",
    ] {
        let mut s = h.control.open_stream().await.unwrap();
        s.write_all(preamble).await.unwrap();
        let mut buf = [0u8; 1];
        assert!(s.read(&mut buf).await.is_err(), "{preamble:?}");
    }
    h.close().await.unwrap();
}

#[tokio::test]
async fn an_over_long_control_line_ends_the_control_stream() {
    let mut h = start();
    let mut c = h.control_stream().await;
    c.write_all(&vec![b'x'; MAX_LINE + 1]).await.unwrap();
    let mut buf = [0u8; 1];
    assert!(c.read(&mut buf).await.is_err());
    h.close().await.unwrap();
}

#[tokio::test]
async fn control_messages_over_the_rate_limit_are_dropped() {
    let mut h = start_with(HostConfig {
        control_burst: 5,
        control_per_second: 1,
        ..HostConfig::default()
    });
    let mut c = h.control_stream().await;
    for pid in 0..50 {
        send(&mut c, &AgentMessage::oom_kill(Some(pid), None)).await;
    }
    c.shutdown().await.unwrap();
    let mut got = 0;
    while let Ok(Some(_)) = tokio::time::timeout(Duration::from_millis(300), h.events.recv()).await
    {
        got += 1;
    }
    assert!((5..=7).contains(&got), "{got} events got through");
    h.close().await.unwrap();
}

#[tokio::test]
async fn drops_still_pending_when_the_control_stream_closes_are_logged() {
    Logs::capture();
    // No refill, so nothing gets through after the burst and no later message reports the count.
    let mut h = start_with(HostConfig {
        control_burst: 5,
        control_per_second: 0,
        ..HostConfig::default()
    });
    let mut c = h.control_stream().await;
    for pid in 0..50 {
        send(&mut c, &AgentMessage::oom_kill(Some(pid), None)).await;
    }
    c.shutdown().await.unwrap();
    while let Ok(Some(_)) = tokio::time::timeout(Duration::from_millis(300), h.events.recv()).await
    {
    }
    h.close().await.unwrap();
    let text = Logs::text();
    assert!(text.contains("dropped=45"), "{text}");
}

#[tokio::test]
async fn a_connection_that_is_not_yamux_is_refused() {
    let (mut guest, host) = tokio::io::duplex(64);
    let server = tokio::spawn(serve_session(
        host,
        name(),
        Arc::new(puddle_types::NullSink),
        Arc::new(Echo),
        HostConfig::default(),
    ));
    guest.write_all(b"GET / HTTP/1.1\r\n\r\n").await.unwrap();
    let err = server.await.unwrap().unwrap_err();
    assert!(matches!(err, SessionError::NotYamux(b'G')), "{err}");
    assert!(err.to_string().contains("0x47"));
}

#[tokio::test(start_paused = true)]
async fn a_silent_connection_times_out() {
    let serve = |host| {
        serve_session(
            host,
            name(),
            Arc::new(puddle_types::NullSink),
            Arc::new(Echo),
            HostConfig {
                first_byte_timeout: Duration::from_secs(1),
                ..HostConfig::default()
            },
        )
    };
    let (guest, host) = tokio::io::duplex(64);
    let result = serve(host).await;
    drop(guest);
    assert!(matches!(result, Err(SessionError::Timeout(_))));
    // A connection closed before any byte is a clean end.
    let (guest, host) = tokio::io::duplex(64);
    drop(guest);
    assert!(serve(host).await.is_ok());
}

#[tokio::test]
async fn a_failing_connection_is_an_io_error() {
    struct Broken;
    impl AsyncRead for Broken {
        fn poll_read(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            _buf: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            Poll::Ready(Err(io::ErrorKind::BrokenPipe.into()))
        }
    }
    impl AsyncWrite for Broken {
        fn poll_write(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            _buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            Poll::Ready(Err(io::ErrorKind::BrokenPipe.into()))
        }
        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }
    let result = serve_session(
        Broken,
        name(),
        Arc::new(puddle_types::NullSink),
        Arc::new(Echo),
        HostConfig::default(),
    )
    .await;
    assert!(matches!(result, Err(SessionError::Io(_))));
}

#[test]
fn rate_limit_refills_at_its_rate_up_to_the_burst() {
    let t0 = Instant::now();
    let mut limit = RateLimit::new(2, 4, t0);
    assert!(limit.allow(t0));
    assert!(limit.allow(t0));
    assert!(!limit.allow(t0));
    assert_eq!(limit.suppressed, 1);
    // A quarter second buys one message at 4/s.
    assert!(limit.allow(t0 + Duration::from_millis(250)));
    assert!(!limit.allow(t0 + Duration::from_millis(250)));
    // A long pause refills to the burst, not beyond.
    let later = t0 + Duration::from_secs(60);
    assert!(limit.allow(later));
    assert!(limit.allow(later));
    assert!(!limit.allow(later));
}

#[test]
fn guest_text_for_logs_is_cut_and_cleaned() {
    assert_eq!(clean("a\u{7}b", 10), "a?b");
    assert_eq!(clean(&"v".repeat(100), MAX_VERSION_CHARS).len(), 64);
}

async fn lookup(h: &mut Harness, name: &str) -> ResolveAnswer {
    let stream = h.control.open_stream().await.unwrap();
    resolve::ask(stream, &ResolveQuery::new(name, RecordType::A), WAIT)
        .await
        .unwrap()
}

#[tokio::test]
async fn a_resolve_stream_gets_the_handlers_answer_and_a_clean_close() {
    let mut h = start();
    assert_eq!(
        lookup(&mut h, "example.com").await,
        ResolveAnswer::NoSuchName { ttl: 11 }
    );
    // A second lookup on the same session works: each lookup has its own stream.
    assert_eq!(
        lookup(&mut h, "a.example").await,
        ResolveAnswer::NoSuchName { ttl: 9 }
    );
    h.close().await.unwrap();
}

#[tokio::test]
async fn a_resolve_stream_with_a_bad_query_is_closed_without_an_answer() {
    let mut h = start();
    for bytes in [
        &b"\0puddle-resolve/1\nnot json\n"[..],
        b"\0puddle-resolve/1\n{\"name\":\"x\",\"type\":\"AAAA\"}\n",
    ] {
        let mut s = h.control.open_stream().await.unwrap();
        s.write_all(bytes).await.unwrap();
        let mut back = Vec::new();
        assert!(s.read_to_end(&mut back).await.is_err() || back.is_empty());
    }
    let mut s = h.control.open_stream().await.unwrap();
    s.write_all(StreamKind::Resolve.preamble()).await.unwrap();
    s.write_all(&vec![b'x'; resolve::MAX_QUERY_LINE + 1])
        .await
        .unwrap();
    let mut back = Vec::new();
    assert!(s.read_to_end(&mut back).await.is_err() || back.is_empty());
    h.close().await.unwrap();
}

#[tokio::test]
async fn a_handler_without_a_name_policy_answers_unavailable() {
    struct Silent;
    impl StreamHandler for Silent {
        async fn handle(&self, _stream: GuestStream) {}
    }
    let (guest, host) = tokio::io::duplex(1 << 20);
    let server = tokio::spawn(serve_session(
        host,
        name(),
        Arc::new(puddle_types::NullSink),
        Arc::new(Silent),
        HostConfig::default(),
    ));
    let mut client = Session::new_client(guest, client_config());
    let mut control = client.control();
    let _driver = tokio::spawn(async move { while client.next().await.is_some() {} });
    let stream = control.open_stream().await.unwrap();
    let answer = resolve::ask(
        stream,
        &ResolveQuery::new("example.com", RecordType::A),
        WAIT,
    )
    .await
    .unwrap();
    assert_eq!(answer, ResolveAnswer::Unavailable);
    control.close().await;
    server.await.unwrap().unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_lookup_that_takes_too_long_is_answered_unavailable() {
    let mut h = start_with(HostConfig {
        resolve_timeout: Duration::from_secs(2),
        ..HostConfig::default()
    });
    assert_eq!(
        lookup(&mut h, "slow.example").await,
        ResolveAnswer::Unavailable
    );
    h.close().await.unwrap();
}

thread_local! {
    static CAPTURED: std::cell::RefCell<Vec<u8>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// What this test's thread logged since [`Logs::capture`].
///
/// One global subscriber writes every thread's log into that thread's own buffer. A subscriber set
/// per test with `set_default` lost events when tests ran in parallel (the last "dropped" warning
/// was missing in about a quarter of runs, never when the test ran alone or with one test thread;
/// the likely cause is tracing's per-call-site cache of whether anyone wants the event). A global
/// subscriber wants every call site. These tests run on a current-thread runtime, so everything a
/// test logs lands in its own thread's buffer.
struct Logs;

impl Logs {
    fn capture() {
        static INSTALL: std::sync::Once = std::sync::Once::new();
        INSTALL.call_once(|| {
            tracing::subscriber::set_global_default(
                tracing_subscriber::fmt()
                    .with_ansi(false)
                    .with_writer(ThreadBuffer)
                    .finish(),
            )
            .expect("no other test installs a global subscriber");
        });
        CAPTURED.with_borrow_mut(Vec::clear);
    }

    fn text() -> String {
        CAPTURED.with_borrow(|buffer| String::from_utf8(buffer.clone()).unwrap())
    }
}

struct ThreadBuffer;

impl io::Write for ThreadBuffer {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        CAPTURED.with_borrow_mut(|buffer| buffer.extend_from_slice(buf));
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for ThreadBuffer {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        Self
    }
}

#[tokio::test(start_paused = true)]
async fn a_lookup_that_takes_too_long_is_logged_with_the_workspace_and_the_limit() {
    Logs::capture();
    let mut h = start_with(HostConfig {
        resolve_timeout: Duration::from_secs(2),
        ..HostConfig::default()
    });
    assert_eq!(
        lookup(&mut h, "slow.example").await,
        ResolveAnswer::Unavailable
    );
    h.close().await.unwrap();
    let text = Logs::text();
    assert!(
        text.contains("WARN") && text.contains("did not answer in time"),
        "{text}"
    );
    assert!(text.contains("timeout_secs=2"), "{text}");
    assert!(text.contains(&name().to_string()), "{text}");
}

#[tokio::test(start_paused = true)]
async fn lookups_over_the_per_session_limit_are_answered_unavailable() {
    let mut h = start_with(HostConfig {
        max_resolves: 2,
        resolve_timeout: Duration::from_secs(30),
        ..HostConfig::default()
    });
    let mut slow = Vec::new();
    for _ in 0..2 {
        let mut s = h.control.open_stream().await.unwrap();
        let q = ResolveQuery::new("slow.example", RecordType::A)
            .to_line()
            .unwrap();
        s.write_all(&[StreamKind::Resolve.preamble(), &q].concat())
            .await
            .unwrap();
        slow.push(s);
    }
    // Let the host take both permits.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        lookup(&mut h, "third.example").await,
        ResolveAnswer::Unavailable
    );
    drop(slow);
    h.close().await.unwrap();
}

/// What a handler is told about the guest's link and ports.
#[derive(Default)]
struct Reach {
    link: std::sync::Mutex<Option<GuestLink>>,
    ports: std::sync::Mutex<Vec<Vec<u16>>>,
}

struct Reaching(Arc<Reach>);

impl StreamHandler for Reaching {
    async fn handle(&self, _stream: GuestStream) {}

    async fn guest_link(&self, link: GuestLink) {
        *self.0.link.lock().unwrap() = Some(link);
    }

    async fn guest_ports(&self, ports: Vec<u16>) {
        self.0.ports.lock().unwrap().push(ports);
    }
}

#[tokio::test]
async fn the_handler_gets_the_link_at_hello_and_the_ports_clamped_to_what_a_port_can_be() {
    let reach = Arc::new(Reach::default());
    let mut h = start_handler(
        Arc::new(Reaching(Arc::clone(&reach))),
        HostConfig::default(),
    );
    let mut c = h.control_stream().await;
    assert!(reach.link.lock().unwrap().is_none());
    send(&mut c, &AgentMessage::hello()).await;
    eventually("the handler gets the link at hello", WAIT, || {
        reach.link.lock().unwrap().is_some()
    })
    .await;
    // A hostile report: unsorted, 600 entries (the most a line holds), then repeats and a zero.
    let many: Vec<u16> = (10_001..=10_600).rev().collect();
    let line = format!(
        "{{\"type\":\"ports\",\"listening\":{}}}\n",
        serde_json::to_string(&many).unwrap()
    );
    assert!(line.len() < MAX_LINE);
    c.write_all(line.as_bytes()).await.unwrap();
    c.write_all(b"{\"type\":\"ports\",\"listening\":[8080,0,3000,8080]}\n")
        .await
        .unwrap();
    let told = eventually("the handler gets both port reports", WAIT, || {
        let ports = reach.ports.lock().unwrap();
        (ports.len() == 2).then(|| ports.clone())
    })
    .await;
    assert_eq!(told[0], (10_001..=10_512).collect::<Vec<u16>>());
    assert_eq!(told[1], [3000, 8080]);
    h.close().await.unwrap();
}

#[tokio::test]
async fn a_forward_opened_through_the_link_is_the_connection_once_the_agent_says_ok() {
    use crate::forward::{Answer, OpenError, parse_request, read_line_exact};
    let reach = Arc::new(Reach::default());
    let mut h = start_handler(
        Arc::new(Reaching(Arc::clone(&reach))),
        HostConfig::default(),
    );
    let mut c = h.control_stream().await;
    send(&mut c, &AgentMessage::hello()).await;
    let link = eventually("the handler gets the link at hello", WAIT, || {
        reach.link.lock().unwrap().clone()
    })
    .await;
    // A fake agent: port 3000 echoes, anything else is refused.
    let mut inbound = std::mem::replace(&mut h.inbound, mpsc::unbounded_channel().1);
    tokio::spawn(async move {
        while let Some(mut stream) = inbound.recv().await {
            tokio::spawn(async move {
                let preamble = read_line_exact(&mut stream, MAX_PREAMBLE).await.unwrap();
                assert_eq!(parse_preamble(&preamble), Ok((StreamKind::Forward, 1)));
                let port = parse_request(&read_line_exact(&mut stream, 16).await.unwrap()).unwrap();
                if port != 3000 {
                    stream
                        .write_all(&Answer::refused("nothing listens").to_line())
                        .await
                        .unwrap();
                    return;
                }
                // The data follows the answer in the same write: it must not be lost.
                let mut reply = Answer::Ok.to_line();
                reply.extend_from_slice(b"hello");
                stream.write_all(&reply).await.unwrap();
                let mut buf = Vec::new();
                stream.read_to_end(&mut buf).await.unwrap();
                stream.write_all(&buf).await.unwrap();
                stream.shutdown().await.unwrap();
            });
        }
    });
    let mut stream = link.open_forward(3000).await.unwrap();
    let mut hello = [0u8; 5];
    stream.read_exact(&mut hello).await.unwrap();
    assert_eq!(&hello, b"hello");
    stream.write_all(b"ping").await.unwrap();
    stream.shutdown().await.unwrap();
    let mut back = Vec::new();
    stream.read_to_end(&mut back).await.unwrap();
    assert_eq!(back, b"ping");
    match link.open_forward(3001).await {
        Err(OpenError::Refused(reason)) => assert_eq!(reason, "nothing listens"),
        other => panic!("{other:?}"),
    }
    h.close().await.unwrap();
    assert!(matches!(
        link.open_forward(3000).await,
        Err(OpenError::Disconnected)
    ));
}
