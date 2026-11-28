// SPDX-License-Identifier: AGPL-3.0-or-later
//! Name lookups through the host session.

use super::*;

pub(super) async fn lookup(h: &mut Harness, name: &str) -> ResolveAnswer {
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
        SessionId::next(),
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
pub(super) struct Logs;

impl Logs {
    pub(super) fn capture() {
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

    pub(super) fn text() -> String {
        CAPTURED.with_borrow(|buffer| String::from_utf8(buffer.clone()).unwrap())
    }
}

pub(super) struct ThreadBuffer;

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
