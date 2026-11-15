// SPDX-License-Identifier: AGPL-3.0-or-later
use proptest::prelude::*;

use super::*;

fn t(host: &str, port: u16, path: Option<&str>) -> RawTarget {
    RawTarget {
        host: host.into(),
        port,
        path: path.map(Into::into),
    }
}

#[test]
fn connect_and_absolute_form_parse() {
    assert_eq!(
        parse_target("CONNECT", "github.com:443"),
        Ok(t("github.com", 443, None))
    );
    assert_eq!(
        parse_target("connect", "github.com:443"),
        Ok(t("github.com", 443, None))
    );
    assert_eq!(
        parse_target("GET", "http://example.com/a/b"),
        Ok(t("example.com", 80, Some("/a/b")))
    );
    assert_eq!(
        parse_target("GET", "HTTP://Example.com:8080?q=1"),
        Ok(t("Example.com", 8080, Some("/?q=1")))
    );
    assert_eq!(
        parse_target("GET", "http://example.com"),
        Ok(t("example.com", 80, Some("/")))
    );
    assert_eq!(
        parse_target("GET", "http://example.com/x#frag"),
        Ok(t("example.com", 80, Some("/x")))
    );
    assert!(parse_target("GET", "/relative").is_err());
    assert!(parse_target("GET", "https://example.com/").is_err());
    assert!(parse_target("GET", "http:/").is_err());
}

#[test]
fn ipv6_literals_need_brackets() {
    assert_eq!(
        parse_target("GET", "http://[::1]:8080/").unwrap(),
        t("::1", 8080, Some("/"))
    );
    assert_eq!(parse_target("GET", "http://[::1]/x").unwrap().port, 80);
    assert_eq!(
        parse_target("CONNECT", "[::ffff:a9fe:a9fe]:443").unwrap(),
        t("::ffff:a9fe:a9fe", 443, None)
    );
    for bad in ["[::1", "[::1]", "[::1]x:1"] {
        assert!(parse_target("CONNECT", bad).is_err(), "{bad}");
    }
    assert!(parse_target("GET", "http://[github.com]/").is_err());
    assert!(parse_target("GET", "http://::1/").is_err());
}

#[test]
fn userinfo_is_refused() {
    assert!(parse_target("GET", "http://user@169.254.169.254/").is_err());
    assert!(parse_target("GET", "http://u:p@github.com/").is_err());
    assert!(parse_target("CONNECT", "u@h:443").is_err());
    // An @ in the path is not userinfo.
    assert!(parse_target("GET", "http://example.com/@x").is_ok());
}

#[test]
fn a_bad_port_is_an_error_not_80() {
    for uri in [
        "http://host:abc/",
        "http://host:65536/",
        "http://host:0/",
        "http://host:-1/",
        "http://host:8o/",
        "http://:80/",
    ] {
        assert!(parse_target("GET", uri).is_err(), "{uri}");
    }
    for uri in ["host:abc", ":443", "host", "host:", "host:99999"] {
        assert!(parse_target("CONNECT", uri).is_err(), "{uri}");
    }
    assert_eq!(parse_target("GET", "http://host:/").unwrap().port, 80);
}

async fn head(bytes: &[u8]) -> Result<Head, HeadError> {
    let mut r = tokio::io::BufReader::new(bytes);
    read_head(&mut r).await.unwrap()
}

#[tokio::test]
async fn heads_over_the_limits_are_too_large() {
    let big = format!("GET http://x/{} HTTP/1.1\r\n\r\n", "a".repeat(MAX_HEAD));
    assert_eq!(head(big.as_bytes()).await, Err(HeadError::TooLarge));
    let no_newline = "G".repeat(10 * 1024 * 1024);
    assert_eq!(head(no_newline.as_bytes()).await, Err(HeadError::TooLarge));
    let many = format!(
        "GET http://x/ HTTP/1.1\r\n{}\r\n",
        "A: b\r\n".repeat(MAX_HEADERS + 5)
    );
    assert_eq!(head(many.as_bytes()).await, Err(HeadError::TooLarge));
    // Just under the byte limit is fine.
    let pad = MAX_HEAD - "GET http://x/ HTTP/1.1\r\nA: \r\n\r\n".len();
    let fits = format!("GET http://x/ HTTP/1.1\r\nA: {}\r\n\r\n", "b".repeat(pad));
    assert!(head(fits.as_bytes()).await.is_ok());
}

#[tokio::test]
async fn a_head_may_hold_exactly_the_most_headers() {
    let with = |n: usize| format!("GET http://x/ HTTP/1.1\r\n{}\r\n", "A: b\r\n".repeat(n));
    assert_eq!(
        head(with(MAX_HEADERS).as_bytes())
            .await
            .unwrap()
            .headers
            .len(),
        MAX_HEADERS
    );
    assert_eq!(
        head(with(MAX_HEADERS + 1).as_bytes()).await,
        Err(HeadError::TooLarge)
    );
}

#[tokio::test]
async fn malformed_heads_are_bad_requests() {
    for bad in [
        &b"GET http://x/ HTTP/1.1\r\nA: \xff\r\n\r\n"[..],
        b"GET http://x/\0 HTTP/1.1\r\n\r\n",
        b"GET http://x/ HTTP/1.1\r\nA: \x1b[31m\r\n\r\n",
        b"GET http://x/ HTTP/1.1",
        b"GET http://x/ HTTP/1.1\r\nA: b\r\n",
        b"GET http://x/ HTTP/2\r\n\r\n",
        b"GET  HTTP/1.1\r\n\r\n",
        b" http://x/ HTTP/1.1\r\n\r\n",
        b"GET a b c HTTP/1.1\r\n\r\n",
        b"GET  http://x/ HTTP/1.1\r\n\r\n",
        b"G(T http://x/ HTTP/1.1\r\n\r\n",
        b"GET http://x/ HTTP/1.1\r\nno colon\r\n\r\n",
        b"GET http://x/ HTTP/1.1\r\nA: b\r\n folded\r\n\r\n",
        b"GET http://x/ HTTP/1.1\r\nBad Name: b\r\n\r\n",
        b"GET http://x/ HTTP/1.1\r\n: empty name\r\n\r\n",
    ] {
        assert!(
            matches!(head(bad).await, Err(HeadError::Bad(_))),
            "{}",
            String::from_utf8_lossy(bad)
        );
    }
    assert_eq!(head(b"").await, Err(HeadError::Empty));
}

#[tokio::test]
async fn a_good_head_parses() {
    let ok = head(b"\r\nGET http://x/ HTTP/1.1\r\nHost: x\nAccept:\t*/*\r\n\r\nrest")
        .await
        .unwrap();
    assert_eq!(ok.method, "GET");
    assert_eq!(ok.uri, "http://x/");
    assert_eq!(ok.version, "HTTP/1.1");
    assert_eq!(ok.headers, vec!["Host: x", "Accept:\t*/*"]);
    assert!(!ok.is_connect());
    let c = head(b"CONNECT a:1 HTTP/1.0\r\n\r\n").await.unwrap();
    assert!(c.is_connect());
}

fn hs(lines: &[&str]) -> Vec<String> {
    lines.iter().map(|l| (*l).to_owned()).collect()
}

#[test]
fn body_framing_refuses_ambiguity() {
    assert_eq!(body_framing(&hs(&["Host: x"])), Ok(Body::None));
    assert_eq!(
        body_framing(&hs(&["Content-Length: 5"])),
        Ok(Body::Length(5))
    );
    assert_eq!(
        body_framing(&hs(&["Content-Length: 5, 5"])),
        Ok(Body::Length(5))
    );
    assert_eq!(
        body_framing(&hs(&["Transfer-Encoding: gzip, chunked"])),
        Ok(Body::Chunked)
    );
    for bad in [
        &["Content-Length: 5", "Content-Length: 6"][..],
        &["Content-Length: 5", "Transfer-Encoding: chunked"],
        &["Transfer-Encoding: chunked, gzip"],
        &["Content-Length: -1"],
        &["Content-Length: +5"],
        &["Content-Length: "],
        &["Content-Length: 99999999999999999999999"],
    ] {
        assert!(body_framing(&hs(bad)).is_err(), "{bad:?}");
    }
}

#[tokio::test]
async fn body_copy_stops_at_the_end_of_the_request() {
    let input = b"hello GET http://other/ HTTP/1.1\r\n\r\n";
    let mut r = tokio::io::BufReader::new(&input[..]);
    let mut out = Vec::new();
    copy_body(&mut r, &mut out, Body::Length(5)).await.unwrap();
    assert_eq!(out, b"hello");

    let chunked = b"5\r\nhello\r\n3;ext=1\r\nabc\r\n0\r\nX-T: 1\r\n\r\n";
    let input = [&chunked[..], b"GET http://other/ HTTP/1.1\r\n\r\n"].concat();
    let mut r = tokio::io::BufReader::new(&input[..]);
    let mut out = Vec::new();
    copy_body(&mut r, &mut out, Body::Chunked).await.unwrap();
    assert_eq!(out, chunked);

    let mut out = Vec::new();
    let mut r = tokio::io::BufReader::new(&b"anything"[..]);
    copy_body(&mut r, &mut out, Body::None).await.unwrap();
    assert_eq!(out.len(), 0);
}

#[tokio::test]
async fn chunk_data_may_hold_blank_lines_and_a_trailer_line_may_be_long() {
    // A blank line inside chunk data is data, not the end of the body.
    let blank = b"2\r\n\r\n\r\n3\r\nabc\r\n0\r\n\r\n";
    let input = [&blank[..], b"NEXT"].concat();
    let mut r = tokio::io::BufReader::new(&input[..]);
    let mut out = Vec::new();
    copy_body(&mut r, &mut out, Body::Chunked).await.unwrap();
    assert_eq!(out, blank);

    // A trailer line longer than a chunk-size line is allowed up to its own limit.
    let long = format!("0\r\nX-T: {}\r\n\r\n", "a".repeat(2000));
    let mut r = tokio::io::BufReader::new(long.as_bytes());
    let mut out = Vec::new();
    copy_body(&mut r, &mut out, Body::Chunked).await.unwrap();
    assert_eq!(out, long.as_bytes());

    // A bare line feed ends a chunk too, and anything else after the data is refused even
    // when a valid body follows it.
    let bare = b"5\nhello\n0\n\n";
    let mut r = tokio::io::BufReader::new(&bare[..]);
    let mut out = Vec::new();
    copy_body(&mut r, &mut out, Body::Chunked).await.unwrap();
    assert_eq!(out, bare);
    let mut r = tokio::io::BufReader::new(&b"5\r\nhelloXX0\r\n\r\n"[..]);
    assert!(
        copy_body(&mut r, &mut Vec::new(), Body::Chunked)
            .await
            .is_err()
    );
}

#[test]
fn a_chunk_size_is_hexadecimal_digits_only() {
    assert_eq!(chunk_size(b"1a;ext=1\r\n").unwrap(), 26);
    for bad in [&b"+5\r\n"[..], b"-5\r\n", b"0x5\r\n", b"\r\n", b";x\r\n"] {
        assert!(chunk_size(bad).is_err(), "{}", String::from_utf8_lossy(bad));
    }
}

#[tokio::test]
async fn broken_bodies_are_errors() {
    for (input, body) in [
        (&b"zz\r\n"[..], Body::Chunked),
        (b"\r\n", Body::Chunked),
        (b"5\r\nhel", Body::Chunked),
        (b"5\r\nhelloXX", Body::Chunked),
        (b"ffffffffffffffffff\r\n", Body::Chunked),
        (b"0\r\nX-T: 1", Body::Chunked),
        (b"abc", Body::Length(5)),
    ] {
        let mut r = tokio::io::BufReader::new(input);
        assert!(
            copy_body(&mut r, &mut Vec::new(), body).await.is_err(),
            "{}",
            String::from_utf8_lossy(input)
        );
    }
}

#[test]
fn upstream_head_rewrites_host_and_drops_hop_headers() {
    let head = Head {
        method: "GET".into(),
        uri: "http://allowed.test/x".into(),
        version: "HTTP/1.1".into(),
        headers: hs(&[
            "Host: denied.test",
            "Connection: keep-alive, X-Secret",
            "X-Secret: 1",
            "Proxy-Authorization: x",
            "Accept: */*",
        ]),
    };
    assert_eq!(
        upstream_head(&head, "/x", "allowed.test", None),
        "GET /x HTTP/1.1\r\nHost: allowed.test\r\nAccept: */*\r\nConnection: close\r\n\r\n"
    );
    assert!(upstream_head(&head, "/x", "[::1]:8080", None).contains("Host: [::1]:8080\r\n"));
    // To a company proxy: the absolute URI and its own credential, never the guest's.
    let proxied = upstream_head(
        &head,
        "http://allowed.test:80/x",
        "allowed.test",
        Some("Basic YTpi"),
    );
    assert!(proxied.starts_with("GET http://allowed.test:80/x HTTP/1.1\r\nHost: allowed.test\r\nProxy-Authorization: Basic YTpi\r\n"));
    assert_eq!(proxied.matches("Proxy-Authorization").count(), 1);
}

#[test]
fn upstream_head_drops_puddle_s_own_header_names_whoever_wrote_them() {
    let head = Head {
        method: "GET".into(),
        uri: "http://allowed.test/x".into(),
        version: "HTTP/1.1".into(),
        headers: hs(&[
            "X-Puddle-Origin: container",
            "x-puddle-protocol: h2",
            "X-PUDDLE-Anything: 1",
            "X-Puddled: kept",
            "Accept: */*",
        ]),
    };
    assert_eq!(
        upstream_head(&head, "/x", "allowed.test", None),
        "GET /x HTTP/1.1\r\nHost: allowed.test\r\nX-Puddled: kept\r\nAccept: */*\r\nConnection: close\r\n\r\n"
    );
}

proptest! {
    /// Whatever the guest sends, the head reader ends with a head or a refusal, never reads
    /// past the limit, and a head it accepts has only clean lines.
    #[test]
    fn read_head_never_panics_and_accepts_only_clean_heads(bytes in proptest::collection::vec(any::<u8>(), 0..4096)) {
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        let got = rt.block_on(async {
            let mut r = tokio::io::BufReader::new(&bytes[..]);
            read_head(&mut r).await.unwrap()
        });
        if let Ok(h) = got {
            prop_assert!(!h.method.is_empty());
            for line in std::iter::once(&h.uri).chain(&h.headers) {
                prop_assert!(!line.bytes().any(|b| b < 0x20 && b != b'\t'));
            }
        }
    }

    #[test]
    fn parse_target_never_panics(method in "(CONNECT|GET|POST)", uri in "\\PC{0,64}") {
        if let Ok(t) = parse_target(&method, &uri) {
            prop_assert!(!t.host.is_empty());
            prop_assert!(t.port != 0);
            prop_assert!(!t.host.contains('@'));
        }
    }

    #[test]
    fn framing_never_panics(lines in proptest::collection::vec("\\PC{0,40}", 0..8)) {
        let _ = body_framing(&lines);
    }
}
