// SPDX-License-Identifier: AGPL-3.0-or-later
use puddle_types::Host;

use super::*;
use crate::target::Target;

fn target() -> Target {
    Target {
        host: Host::parse_normalised("bound.test").unwrap(),
        port: 443,
    }
}

fn builder() -> ::http::request::Builder {
    Request::builder().uri("https://bound.test/a?b=1")
}

fn status(refusal: &Refusal) -> &str {
    refusal.status.split(' ').next().unwrap()
}

#[test]
fn a_request_for_the_connection_host_passes_with_its_path_and_query() {
    let request = builder().body(()).unwrap();
    let checked = check(&request, &target()).unwrap();
    assert_eq!(checked.method, Method::GET);
    assert_eq!(checked.path, "/a?b=1");
    assert!(checked.protocol.is_none());
    // `Host` and `:authority` may both be there if they agree, with or without the port.
    for host in ["bound.test", "Bound.Test:443"] {
        let request = builder().header("host", host).body(()).unwrap();
        assert!(check(&request, &target()).is_ok(), "{host}");
    }
}

#[test]
fn a_request_for_another_host_or_in_another_form_is_refused() {
    for (what, request, code) in [
        (
            "another authority",
            Request::builder()
                .uri("https://other.test/")
                .body(())
                .unwrap(),
            "421",
        ),
        (
            "another port",
            Request::builder()
                .uri("https://bound.test:8443/")
                .body(())
                .unwrap(),
            "421",
        ),
        (
            "a Host that disagrees",
            builder().header("host", "other.test").body(()).unwrap(),
            "421",
        ),
        (
            "two Host headers",
            builder()
                .header("host", "bound.test")
                .header("host", "bound.test")
                .body(())
                .unwrap(),
            "400",
        ),
        (
            "a Host that is not text",
            builder()
                .header("host", HeaderValue::from_bytes(b"bound\xff.test").unwrap())
                .body(())
                .unwrap(),
            "400",
        ),
        (
            "no authority at all",
            Request::builder().uri("/a").body(()).unwrap(),
            "400",
        ),
        (
            "userinfo",
            Request::builder()
                .uri("https://user@bound.test/")
                .body(())
                .unwrap(),
            "400",
        ),
        (
            "http scheme",
            Request::builder()
                .uri("http://bound.test/")
                .body(())
                .unwrap(),
            "400",
        ),
    ] {
        let refusal = check(&request, &target()).unwrap_err();
        assert_eq!(status(&refusal), code, "{what}: {}", refusal.message);
    }
}

#[test]
fn headers_that_do_not_exist_in_http2_are_refused() {
    for (name, value) in [
        ("connection", "keep-alive"),
        ("keep-alive", "timeout=5"),
        ("proxy-connection", "keep-alive"),
        ("transfer-encoding", "chunked"),
        ("upgrade", "h2c"),
        ("te", "gzip"),
    ] {
        let request = builder().header(name, value).body(()).unwrap();
        let refusal = check(&request, &target()).unwrap_err();
        assert_eq!(status(&refusal), "400", "{name}");
    }
    let request = builder().header("te", "Trailers").body(()).unwrap();
    assert!(check(&request, &target()).is_ok());
}

#[test]
fn content_length_and_expect_are_checked() {
    for (values, ok) in [
        (vec!["10"], true),
        (vec!["10", "10"], true),
        (vec!["10", "11"], false),
        (vec!["ten"], false),
        (vec!["-1"], false),
    ] {
        let mut request = builder();
        for value in &values {
            request = request.header("content-length", *value);
        }
        let result = check(&request.body(()).unwrap(), &target());
        assert_eq!(result.is_ok(), ok, "{values:?}");
    }
    let request = builder().header("expect", "100-continue").body(()).unwrap();
    assert!(check(&request, &target()).is_ok());
    let request = builder().header("expect", "gimme").body(()).unwrap();
    assert_eq!(status(&check(&request, &target()).unwrap_err()), "417");
}

#[test]
fn connect_is_for_websockets_only() {
    let plain = Request::builder()
        .method("CONNECT")
        .uri("https://bound.test/")
        .body(())
        .unwrap();
    assert_eq!(status(&check(&plain, &target()).unwrap_err()), "400");
    let websocket = Request::builder()
        .method("CONNECT")
        .uri("https://bound.test/chat")
        .extension(hyper::ext::Protocol::from_static("websocket"))
        .body(())
        .unwrap();
    let checked = check(&websocket, &target()).unwrap();
    assert_eq!(
        checked.protocol.as_ref().map(hyper::ext::Protocol::as_str),
        Some("websocket")
    );
    let stray = builder()
        .extension(hyper::ext::Protocol::from_static("websocket"))
        .body(())
        .unwrap();
    assert_eq!(status(&check(&stray, &target()).unwrap_err()), "400");
}

#[test]
fn the_injector_sees_the_lines_of_an_http11_head_with_the_host() {
    let mut headers = HeaderMap::new();
    headers.insert("host", HeaderValue::from_static("bound.test"));
    headers.append("cookie", HeaderValue::from_static("a=1"));
    headers.append("cookie", HeaderValue::from_static("b=2"));
    headers.insert("x-bytes", HeaderValue::from_bytes(b"a\xffb").unwrap());
    let lines = header_lines(&headers, "bound.test");
    assert_eq!(lines.first().map(String::as_str), Some("host: bound.test"));
    assert_eq!(lines.iter().filter(|l| l.starts_with("host:")).count(), 1);
    assert_eq!(lines.iter().filter(|l| l.starts_with("cookie:")).count(), 2);
    assert!(lines.iter().any(|l| l.starts_with("x-bytes: a")));
}

#[test]
fn a_refusal_becomes_a_response_with_its_status_headers_and_text() {
    let refusal = Refusal::new("403 Forbidden", "nope").header("x-puddle-blocked", "why");
    let response = refusal_response(&refusal);
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(response.headers()["x-puddle-blocked"], "why");
    assert_eq!(
        response.headers()["content-type"],
        "text/plain; charset=utf-8"
    );
    let odd = refusal_response(&Refusal::new("garbage", "x"));
    assert_eq!(odd.status(), StatusCode::BAD_GATEWAY);
}

#[test]
fn trailers_lose_framing_routing_authentication_and_injected_names() {
    let mut trailers = HeaderMap::new();
    for name in [
        "x-kept",
        "grpc-status",
        "authorization",
        "cookie",
        "host",
        "content-length",
        "transfer-encoding",
        "x-api-key",
    ] {
        trailers.insert(
            HeaderName::from_bytes(name.as_bytes()).unwrap(),
            "v".parse().unwrap(),
        );
    }
    scrub_trailers(&mut trailers, &[HeaderName::from_static("x-api-key")]);
    let mut left: Vec<&str> = trailers.keys().map(HeaderName::as_str).collect();
    left.sort_unstable();
    assert_eq!(left, ["grpc-status", "x-kept"]);
}

#[test]
fn connection_specific_response_headers_are_dropped() {
    for name in [
        "connection",
        "keep-alive",
        "transfer-encoding",
        "upgrade",
        "te",
        "trailer",
    ] {
        assert!(
            is_connection_specific(&HeaderName::from_static(name)),
            "{name}"
        );
    }
    assert!(!is_connection_specific(&HeaderName::from_static(
        "content-type"
    )));
}

#[test]
fn a_response_loses_the_headers_of_its_http11_connection_and_those_it_names() {
    let mut headers = HeaderMap::new();
    headers.append("connection", "keep-alive, X-Hop".parse().unwrap());
    headers.append("keep-alive", "timeout=5".parse().unwrap());
    headers.append("x-hop", "only for this connection".parse().unwrap());
    headers.append("x-kept", "one".parse().unwrap());
    headers.append("x-kept", "two".parse().unwrap());
    headers.append("content-type", "text/plain".parse().unwrap());
    let kept = end_to_end_headers(&headers);
    let mut names: Vec<&str> = kept.keys().map(HeaderName::as_str).collect();
    names.sort_unstable();
    assert_eq!(names, ["content-type", "x-kept"]);
    assert_eq!(kept.get_all("x-kept").iter().count(), 2);
}

#[test]
fn the_refusal_without_random_bytes_is_a_bad_gateway() {
    assert_eq!(no_random_bytes().status, "502 Bad Gateway");
}

#[tokio::test]
async fn the_sent_flag_wakes_its_waiters_once_and_stays_set() {
    let sent = Arc::new(Sent::default());
    let waiter = {
        let sent = Arc::clone(&sent);
        tokio::spawn(async move { sent.wait().await })
    };
    tokio::task::yield_now().await;
    assert!(!sent.is_done());
    assert!(
        !waiter.is_finished(),
        "a waiter returned before the send finished"
    );
    sent.finish();
    waiter.await.unwrap();
    sent.wait().await;
}

/// A body that yields the frames it is given, then ends or never answers.
struct Scripted {
    frames: std::collections::VecDeque<Result<Frame<Bytes>, io::Error>>,
    exact: Option<u64>,
    then_pending: bool,
}

impl Scripted {
    fn new(frames: Vec<Result<Frame<Bytes>, io::Error>>, exact: Option<u64>) -> Self {
        Self {
            frames: frames.into(),
            exact,
            then_pending: false,
        }
    }

    fn then_pending(mut self) -> Self {
        self.then_pending = true;
        self
    }
}

impl Body for Scripted {
    type Data = Bytes;
    type Error = io::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        _cx: &mut TaskContext<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, io::Error>>> {
        match self.frames.pop_front() {
            Some(frame) => Poll::Ready(Some(frame)),
            None if self.then_pending => Poll::Pending,
            None => Poll::Ready(None),
        }
    }

    fn is_end_stream(&self) -> bool {
        self.frames.is_empty() && !self.then_pending
    }

    fn size_hint(&self) -> SizeHint {
        self.exact
            .map_or_else(SizeHint::default, SizeHint::with_exact)
    }
}

fn data(text: &'static str) -> Frame<Bytes> {
    Frame::data(Bytes::from_static(text.as_bytes()))
}

const IDLE: Duration = Duration::from_secs(3600);

fn guest_body(inner: Scripted) -> (GuestBody<Scripted>, Arc<Sent>) {
    let (body, sent, _) = guest_body_in(inner, &Arc::new(Registry::default()));
    (body, sent)
}

fn guest_body_in(
    inner: Scripted,
    registry: &Arc<Registry>,
) -> (GuestBody<Scripted>, Arc<Sent>, StreamGuard) {
    let sent = Arc::new(Sent::default());
    let guard = registry.open(IDLE, None);
    let body = GuestBody::new(
        inner,
        &guard.activity(),
        &sent,
        vec![HeaderName::from_static("x-api-key")],
    );
    (body, sent, guard)
}

fn upstream_body(inner: Scripted, sent: &Arc<Sent>) -> (UpstreamBody<Scripted>, Arc<Registry>) {
    let registry = Arc::new(Registry::default());
    let body = UpstreamBody::new(inner, sent, None, registry.open(IDLE, None));
    (body, registry)
}

#[tokio::test]
async fn a_request_body_that_ends_short_of_its_content_length_is_an_error() {
    let (mut body, sent) = guest_body(Scripted::new(vec![Ok(data("12345"))], Some(10)));
    assert!(body.frame().await.unwrap().unwrap().is_data());
    let err = body.frame().await.unwrap().unwrap_err();
    assert!(
        err.to_string().contains("before its Content-Length"),
        "{err}"
    );
    assert!(
        !sent.is_done(),
        "a truncated request is never marked as sent"
    );
}

#[tokio::test]
async fn a_request_body_with_its_whole_length_ends_and_is_marked_sent() {
    let (mut body, sent) = guest_body(Scripted::new(
        vec![Ok(data("12")), Ok(data("345"))],
        Some(5),
    ));
    assert!(body.frame().await.unwrap().unwrap().is_data());
    assert!(body.frame().await.unwrap().unwrap().is_data());
    assert!(body.frame().await.is_none());
    assert!(sent.is_done());
    assert_eq!(body.size_hint().exact(), Some(5));
}

#[tokio::test]
async fn a_request_body_without_a_declared_length_ends_at_its_end() {
    let (mut body, sent) = guest_body(Scripted::new(vec![Ok(data("12"))], None));
    assert!(body.frame().await.unwrap().unwrap().is_data());
    assert!(body.frame().await.is_none());
    assert!(sent.is_done());
}

#[tokio::test]
async fn an_error_in_the_request_body_is_passed_on() {
    let (mut body, _) = guest_body(Scripted::new(
        vec![Err(io::Error::other("the guest's stream broke"))],
        None,
    ));
    let err = body.frame().await.unwrap().unwrap_err();
    assert!(err.to_string().contains("stream broke"), "{err}");
}

/// Runs the connection's sweep every tick for `span`, as the connection loop does.
async fn sweep_for(registry: &Registry, span: Duration) {
    let mut waited = Duration::ZERO;
    while waited < span {
        tokio::time::advance(SWEEP_INTERVAL).await;
        waited += SWEEP_INTERVAL;
        registry.sweep_for_test(Instant::now());
    }
}

#[tokio::test(start_paused = true)]
async fn a_request_body_that_moves_nothing_for_an_hour_is_reset_by_the_sweep() {
    let registry = Arc::new(Registry::default());
    let (mut body, _, _guard) =
        guest_body_in(Scripted::new(vec![], None).then_pending(), &registry);
    let frame = tokio::spawn(async move { body.frame().await });
    tokio::task::yield_now().await;
    assert!(!frame.is_finished());
    sweep_for(&registry, IDLE + Duration::from_secs(4)).await;
    let err = frame.await.unwrap().unwrap().unwrap_err();
    let err = err.downcast_ref::<io::Error>().unwrap();
    assert_eq!(err.kind(), io::ErrorKind::TimedOut);
}

#[tokio::test(start_paused = true)]
async fn a_request_body_that_is_polled_is_taken_up_and_one_that_is_not_is_waited_for() {
    let (mut body, sent) = guest_body(Scripted::new(vec![Ok(data("a"))], None).then_pending());
    let waiting = tokio::time::timeout(Duration::from_secs(5), sent.started()).await;
    assert!(waiting.is_err(), "nothing polled the body yet");
    assert!(body.frame().await.is_some());
    sent.started().await;
    assert!(!sent.is_done(), "taken up is not sent in full");
}

#[tokio::test]
async fn an_empty_request_body_is_sent_at_once() {
    let (body, sent) = guest_body(Scripted::new(vec![], Some(0)));
    assert!(body.is_end_stream());
    assert!(sent.is_done());
}

#[tokio::test]
async fn a_response_body_passes_its_frames_and_counts_the_stream_until_it_is_dropped() {
    let sent = Arc::new(Sent::default());
    sent.finish();
    let (mut body, open) = upstream_body(Scripted::new(vec![Ok(data("ab"))], Some(2)), &sent);
    assert_eq!(open.len(), 1);
    assert_eq!(body.size_hint().exact(), Some(2));
    assert!(body.frame().await.unwrap().unwrap().is_data());
    assert!(body.frame().await.is_none());
    assert!(body.is_end_stream());
    drop(body);
    assert_eq!(open.len(), 0);
}

#[tokio::test]
async fn an_error_in_the_response_body_is_passed_on() {
    let sent = Arc::new(Sent::default());
    let (mut body, _) = upstream_body(
        Scripted::new(vec![Err(io::Error::other("the server broke"))], None),
        &sent,
    );
    let err = body.frame().await.unwrap().unwrap_err();
    assert!(err.to_string().contains("server broke"), "{err}");
}

#[tokio::test(start_paused = true)]
async fn a_response_body_that_moves_nothing_for_an_hour_is_reset_by_the_sweep() {
    let sent = Arc::new(Sent::default());
    let (mut body, registry) = upstream_body(Scripted::new(vec![], None).then_pending(), &sent);
    let frame = tokio::spawn(async move { body.frame().await });
    tokio::task::yield_now().await;
    sweep_for(&registry, IDLE + Duration::from_secs(4)).await;
    let err = frame.await.unwrap().unwrap().unwrap_err();
    let err = err.downcast_ref::<io::Error>().unwrap();
    assert_eq!(err.kind(), io::ErrorKind::TimedOut);
}

#[test]
fn a_target_that_is_not_a_path_is_refused() {
    let request = Request::builder()
        .method(Method::OPTIONS)
        .uri("*")
        .header("host", "bound.test")
        .body(())
        .unwrap();
    let refusal = check(&request, &target()).unwrap_err();
    assert_eq!(status(&refusal), "400");
}
