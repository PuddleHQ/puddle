// SPDX-License-Identifier: AGPL-3.0-or-later
use std::time::Duration;

use futures_util::StreamExt;
use puddle_test_kit::{closes_without_event, eventually};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_yamux::{Control, Session};

use super::*;
use crate::control::{PREAMBLE, PROTOCOL_VERSION};
use crate::forward::{Answer, OpenError, read_line_exact};
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

    /// Answers an agent stream with `agent:` and what it read, so a test sees every byte arrive.
    async fn ssh_agent(&self, mut stream: AgentStream) {
        let mut buf = Vec::new();
        if stream.read_to_end(&mut buf).await.is_ok() {
            let _ = stream.write_all(b"agent:").await;
            let _ = stream.write_all(&buf).await;
            let _ = stream.flush().await;
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

fn name() -> WorkspaceId {
    WorkspaceId::new("box").unwrap()
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
    reports: std::sync::Mutex<Vec<AgentReport>>,
}

struct Listening(Arc<Told>);

impl StreamHandler for Listening {
    async fn handle(&self, _stream: GuestStream) {}

    async fn udp_refused(&self, refusal: UdpRefusal) {
        self.0.udp.lock().unwrap().push(refusal);
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
    send(&mut c, &AgentMessage::capture_unavailable(String::new())).await;
    send(&mut c, &AgentMessage::capture_ready()).await;
    // The session is still reading: wait until the last report was handled.
    eventually(
        "all five capture reports handled",
        Duration::from_secs(10),
        || told.reports.lock().unwrap().len() == 5,
    )
    .await;
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
    let reports = told.reports.lock().unwrap().clone();
    assert_eq!(reports.len(), 5, "{reports:?}");
    assert_eq!(
        reports[..2],
        [
            AgentReport::CaptureTampered {
                table_missing: true
            },
            AgentReport::CaptureTampered {
                table_missing: false
            }
        ]
    );
    let AgentReport::CaptureUnavailable {
        reason: Some(reason),
    } = &reports[2]
    else {
        panic!("wrong report: {:?}", reports[2]);
    };
    assert!(reason.chars().count() <= MAX_REASON_CHARS && !reason.contains('\u{1b}'));
    assert!(reason.starts_with("no nf_tables?[2J"));
    assert_eq!(
        reports[3],
        AgentReport::CaptureUnavailable {
            reason: Some("no reason given".into())
        },
        "an empty reason is still an outage"
    );
    assert_eq!(reports[4], AgentReport::CaptureUnavailable { reason: None });
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
    eventually(
        "three agent reports handled",
        Duration::from_secs(10),
        || told.reports.lock().unwrap().len() == 3,
    )
    .await;
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
async fn an_editor_status_reaches_the_handler_cleaned_and_a_blank_one_means_installed() {
    let told = Arc::new(Told::default());
    let mut h = start_handler(
        Arc::new(Listening(Arc::clone(&told))),
        HostConfig::default(),
    );
    let mut c = h.control_stream().await;
    send(
        &mut c,
        &AgentMessage::editor_status(
            EditorProblemKind::Install,
            Some("the download did not match\u{1b}[2J"),
        ),
    )
    .await;
    // A line from a hostile guest, longer than the agent would send.
    let long = format!(
        "{{\"type\":\"editor_status\",\"problem\":\"{}\"}}\n",
        "x".repeat(900)
    );
    c.write_all(long.as_bytes()).await.unwrap();
    send(
        &mut c,
        &AgentMessage::editor_status(EditorProblemKind::Install, Some("")),
    )
    .await;
    send(
        &mut c,
        &AgentMessage::editor_status(EditorProblemKind::Install, None),
    )
    .await;
    let reports = eventually("the handler gets all four editor statuses", WAIT, || {
        let reports = told.reports.lock().unwrap().clone();
        (reports.len() == 4).then_some(reports)
    })
    .await;
    assert_eq!(
        reports[0],
        AgentReport::EditorStatus {
            kind: EditorProblemKind::Install,
            problem: Some("the download did not match?[2J".into())
        }
    );
    let AgentReport::EditorStatus {
        problem: Some(text),
        ..
    } = &reports[1]
    else {
        panic!("{reports:?}");
    };
    assert_eq!(text.chars().count(), MAX_EDITOR_PROBLEM_CHARS);
    assert_eq!(
        reports[2],
        AgentReport::EditorStatus {
            kind: EditorProblemKind::Install,
            problem: None,
        }
    );
    assert_eq!(
        reports[3],
        AgentReport::EditorStatus {
            kind: EditorProblemKind::Install,
            problem: None,
        }
    );
    h.close().await.unwrap();
}

#[tokio::test]
async fn an_update_problem_arrives_as_its_own_kind_and_an_unknown_kind_is_dropped() {
    let told = Arc::new(Told::default());
    let mut h = start_handler(
        Arc::new(Listening(Arc::clone(&told))),
        HostConfig::default(),
    );
    let mut c = h.control_stream().await;
    send(
        &mut c,
        &AgentMessage::editor_status(EditorProblemKind::Update, Some("rolled back")),
    )
    .await;
    c.write_all(b"{\"type\":\"editor_status\",\"kind\":\"later\",\"problem\":\"x\"}\n")
        .await
        .unwrap();
    send(
        &mut c,
        &AgentMessage::editor_status(EditorProblemKind::Update, None),
    )
    .await;
    eventually("both update reports arrive", WAIT, || {
        told.reports.lock().unwrap().len() == 2
    })
    .await;
    assert_eq!(
        told.reports.lock().unwrap().clone(),
        vec![
            AgentReport::EditorStatus {
                kind: EditorProblemKind::Update,
                problem: Some("rolled back".into())
            },
            AgentReport::EditorStatus {
                kind: EditorProblemKind::Update,
                problem: None
            },
        ]
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

#[test]
fn guest_text_loses_the_characters_that_reorder_or_hide_text() {
    let reason = clean(
        "safe\u{202e}txt.exe\u{200b}\u{feff}\u{2066}",
        MAX_REASON_CHARS,
    );
    assert_eq!(reason, "safe?txt.exe???");
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
    let lines = format!(
        "{{\"type\":\"future_thing\"}}\nnot json\n{{\"type\":\"hello\",\"agent_version\":\"\\u0007x\",\"protocol\":{PROTOCOL_VERSION}}}\n"
    );
    c.write_all(lines.as_bytes()).await.unwrap();
    send(&mut c, &AgentMessage::oom_kill(Some(1), Some("a".into()))).await;
    assert_eq!(h.next_event().await, Event::oom_kill(name(), 1, "a"));
    h.close().await.unwrap();
}

/// A handler that records the link, the ports and the reports.
#[derive(Default)]
struct Heard {
    reach: Reach,
    told: Told,
}

struct Hearing(Arc<Heard>);

impl StreamHandler for Hearing {
    async fn handle(&self, _stream: GuestStream) {}

    async fn guest_link(&self, link: GuestLink) {
        *self.0.reach.link.lock().unwrap() = Some(link);
    }

    async fn guest_ports(&self, ports: Vec<u16>) {
        self.0.reach.ports.lock().unwrap().push(ports);
    }

    async fn agent_report(&self, report: AgentReport) {
        self.0.told.reports.lock().unwrap().push(report);
    }
}

#[tokio::test]
async fn a_guest_agent_on_another_protocol_is_reported_once_and_nothing_else_it_sends_counts() {
    Logs::capture();
    let heard = Arc::new(Heard::default());
    let mut h = start_handler(Arc::new(Hearing(Arc::clone(&heard))), HostConfig::default());
    let mut c = h.control_stream().await;
    let other = AgentMessage::Hello {
        agent_version: "0.9.0\u{7}".to_owned(),
        protocol: PROTOCOL_VERSION + 1,
    };
    send(&mut c, &other).await;
    send(&mut c, &AgentMessage::oom_kill(Some(1), Some("a".into()))).await;
    c.write_all(b"{\"type\":\"ports\",\"listening\":[8080]}\n")
        .await
        .unwrap();
    send(&mut c, &AgentMessage::capture_tampered(true)).await;
    // A later hello on this protocol does not take the refusal back: the stream's first word stands.
    send(&mut c, &AgentMessage::hello()).await;
    let reports = eventually("the mismatch is reported", WAIT, || {
        let reports = heard.told.reports.lock().unwrap();
        (!reports.is_empty()).then(|| reports.clone())
    })
    .await;
    assert_eq!(
        reports,
        [AgentReport::ProtocolMismatch {
            agent_version: "0.9.0?".to_owned(),
            protocol: PROTOCOL_VERSION + 1,
        }]
    );

    // The refused stream stays open (an agent whose stream closed would reopen it in a loop), so a
    // second control stream is still turned away.
    let mut second = h.control_stream().await;
    send(&mut second, &AgentMessage::hello()).await;
    let mut buf = [0u8; 1];
    assert!(second.read(&mut buf).await.is_err());

    let mut events = std::mem::replace(&mut h.events, mpsc::unbounded_channel().1);
    h.close().await.unwrap();
    closes_without_event(
        "an event from an agent on another protocol",
        Duration::from_millis(200),
        &mut events,
    )
    .await;
    assert!(
        heard.reach.link.lock().unwrap().is_none(),
        "the host got a link to an agent it does not speak with"
    );
    assert!(heard.reach.ports.lock().unwrap().is_empty());
    assert_eq!(heard.told.reports.lock().unwrap().len(), 1);
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

use link::Reach;
use lookups::Logs;

mod link;
mod lookups;
mod streams;
