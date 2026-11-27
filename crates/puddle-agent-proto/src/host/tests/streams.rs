// SPDX-License-Identifier: AGPL-3.0-or-later
//! Stream-level protocol tests: control and kind handling, rate limits and timeouts.

use super::lookups::lookup;
use super::*;

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
        b"\0puddle-ssh-agent/2\n",
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

#[tokio::test(start_paused = true)]
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
    assert_eq!(got, 5, "the burst, and nothing at the same instant");
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

/// Reads one byte from `stream` and reports whether the host ended it (end of stream or a reset)
/// within `within` of virtual time; a stream the host left open fails the test.
async fn ended_within(stream: &mut StreamHandle, within: Duration) {
    let mut buf = [0u8; 1];
    match tokio::time::timeout(within, stream.read(&mut buf)).await {
        Err(elapsed) => panic!("the host kept the stream open for {within:?}: {elapsed}"),
        Ok(Ok(0) | Err(_)) => {}
        Ok(Ok(_)) => panic!("the host sent data on a stream it should have closed"),
    }
}

fn quick_waits() -> HostConfig {
    HostConfig {
        first_byte_timeout: Duration::from_secs(2),
        ..HostConfig::default()
    }
}

#[tokio::test(start_paused = true)]
async fn a_stream_that_stays_silent_after_opening_is_closed_at_the_first_byte_limit() {
    let mut h = start_with(quick_waits());
    let mut silent = h.control.open_stream().await.unwrap();
    // Nothing goes out on it, not even the byte that tells the host what kind it is.
    ended_within(&mut silent, Duration::from_secs(5)).await;
    // The session carries on: a control stream still reports.
    let mut c = h.control_stream().await;
    send(&mut c, &AgentMessage::oom_kill(Some(1), None)).await;
    assert_eq!(h.next_event().await, Event::oom_kill(name(), 1, "unknown"));
    h.close().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn half_a_preamble_is_closed_at_the_limit_and_does_not_hold_the_control_slot() {
    let mut h = start_with(quick_waits());
    let mut half = h.control.open_stream().await.unwrap();
    half.write_all(&PREAMBLE[..PREAMBLE.len() - 3])
        .await
        .unwrap();
    ended_within(&mut half, Duration::from_secs(5)).await;
    // The cut preamble never claimed the single control slot.
    let mut c = h.control_stream().await;
    send(&mut c, &AgentMessage::oom_kill(Some(2), None)).await;
    assert_eq!(h.next_event().await, Event::oom_kill(name(), 2, "unknown"));
    h.close().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn a_resolve_stream_that_never_sends_its_query_is_closed_at_the_limit() {
    let mut h = start_with(quick_waits());
    let mut s = h.control.open_stream().await.unwrap();
    s.write_all(StreamKind::Resolve.preamble()).await.unwrap();
    ended_within(&mut s, Duration::from_secs(5)).await;
    // It held no lookup slot: a real query is still answered.
    assert!(matches!(
        lookup(&mut h, "example.com").await,
        ResolveAnswer::NoSuchName { .. }
    ));
    h.close().await.unwrap();
}

#[tokio::test]
async fn a_guest_cannot_open_the_kinds_only_the_host_opens() {
    let mut h = start();
    for kind in [
        StreamKind::Forward,
        StreamKind::Connect,
        StreamKind::HostControl,
        StreamKind::Editor,
    ] {
        let mut s = h.control.open_stream().await.unwrap();
        s.write_all(kind.preamble()).await.unwrap();
        s.write_all(b"payload\n").await.unwrap();
        let mut buf = [0u8; 1];
        assert!(
            !matches!(s.read(&mut buf).await, Ok(n) if n > 0),
            "{kind:?} got an answer"
        );
    }
    // None of them reached the handler as a proxied connection, and the session still serves.
    let mut c = h.control_stream().await;
    send(&mut c, &AgentMessage::oom_kill(Some(3), None)).await;
    assert_eq!(h.next_event().await, Event::oom_kill(name(), 3, "unknown"));
    h.close().await.unwrap();
}
