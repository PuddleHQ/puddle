// SPDX-License-Identifier: AGPL-3.0-or-later
use std::io::{Read, Write};
use std::net::TcpListener;

use super::*;

/// Serves `routes` (path to body) over HTTP on localhost until the test process ends; returns its
/// base URL. An unlisted path answers 404.
pub(crate) fn serve(routes: &'static [(&'static str, &'static [u8])]) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let mut buf = [0_u8; 4096];
            let n = stream.read(&mut buf).unwrap_or(0);
            let head = String::from_utf8_lossy(&buf[..n]).into_owned();
            let path = head.split_whitespace().nth(1).unwrap_or("");
            let reply = routes.iter().find(|(p, _)| *p == path);
            let (status, body) = reply.map_or(("404 Not Found", &b""[..]), |(_, b)| ("200 OK", *b));
            let _ = write!(
                stream,
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(body);
        }
    });
    base
}

const ROUTES: &[(&str, &[u8])] = &[
    ("/repos/f/m/commits/v1", b"{\"sha\":\"abc\"}"),
    ("/repos/f/m/contents/Makefile", b"raw file"),
    ("/f/m/releases/download/v1/asset.bin", b"\x00\x01payload"),
];

#[test]
fn reads_an_api_answer_and_a_release_asset() {
    let base = serve(ROUTES);
    let gh = GitHub::at(&base, &base, "curl");
    assert_eq!(
        gh.api("repos/f/m/commits/v1", false).unwrap(),
        "{\"sha\":\"abc\"}"
    );
    assert_eq!(
        gh.api("repos/f/m/contents/Makefile", true).unwrap(),
        "raw file"
    );
    let dir = tempfile::tempdir().unwrap();
    let got = gh
        .release_asset("f/m", "v1", "asset.bin", dir.path())
        .unwrap();
    assert_eq!(got, dir.path().join("asset.bin"));
    assert_eq!(std::fs::read(&got).unwrap(), b"\x00\x01payload");
}

#[test]
fn a_missing_asset_fails_and_leaves_no_file() {
    let base = serve(ROUTES);
    let gh = GitHub::at(&base, &base, "curl");
    let dir = tempfile::tempdir().unwrap();
    let err = gh
        .release_asset("f/m", "v1", "gone.bin", dir.path())
        .unwrap_err()
        .to_string();
    assert!(err.contains("404"), "{err}");
    assert!(!dir.path().join("gone.bin").exists());
    assert!(gh.api("repos/f/m/commits/none", false).is_err());
}

#[test]
fn a_host_without_curl_is_told_what_to_install() {
    // An absolute path is never searched on PATH, so a Windows folder on PATH (permission denied
    // instead of not found) cannot change the error.
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("puddle-no-such-curl");
    let name = missing.display();
    let gh = GitHub::at("http://127.0.0.1:1", "http://127.0.0.1:1", &missing);
    for err in [
        gh.api("repos/f/m/commits/v1", false)
            .unwrap_err()
            .to_string(),
        gh.release_asset("f/m", "v1", "a", dir.path())
            .unwrap_err()
            .to_string(),
    ] {
        assert!(err.contains(&format!("`{name}` is not installed")), "{err}");
        assert!(err.contains("install it"), "{err}");
    }
}

#[test]
fn the_public_client_is_https_only() {
    let args: Vec<_> = GitHub::public()
        .curl()
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    assert!(
        args.windows(2).any(|w| w == ["--proto", "=https"]),
        "{args:?}"
    );
}

#[test]
fn every_request_has_a_connect_and_a_total_time_limit() {
    for client in [GitHub::public(), GitHub::at("http://x", "http://x", "curl")] {
        let args: Vec<_> = client
            .curl()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        for flag in ["--connect-timeout", "--max-time"] {
            assert!(args.iter().any(|a| a == flag), "{flag} missing: {args:?}");
        }
    }
}

/// Answers each request with the next of `replies` (status line and extra headers, body), the last
/// one again when they run out; returns the base URL and every request head seen so far.
fn serve_script(
    replies: &'static [(&'static str, &'static str)],
) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let log = std::sync::Arc::clone(&seen);
    std::thread::spawn(move || {
        for (n, mut stream) in listener.incoming().flatten().enumerate() {
            let mut buf = [0_u8; 4096];
            let read = stream.read(&mut buf).unwrap_or(0);
            log.lock()
                .unwrap()
                .push(String::from_utf8_lossy(&buf[..read]).into_owned());
            let (head, body) = replies[n.min(replies.len() - 1)];
            let _ = write!(
                stream,
                "HTTP/1.1 {head}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    (base, seen)
}

const LIMITED: (&str, &str) = (
    "403 Forbidden\r\nx-ratelimit-limit: 60\r\nx-ratelimit-remaining: 0\r\nx-ratelimit-reset: 1700000000",
    "{\"message\":\"API rate limit exceeded for 10.0.0.1\"}",
);

#[test]
fn the_token_goes_in_an_authorization_header_and_not_on_the_command_line() {
    let (base, seen) = serve_script(&[("200 OK", "{}")]);
    let gh = GitHub::at(&base, &base, "curl").with_token("s3cret-token");
    assert_eq!(gh.api("repos/f/m/commits/v1", false).unwrap(), "{}");
    let heads = seen.lock().unwrap();
    assert!(
        heads[0]
            .to_ascii_lowercase()
            .contains("authorization: bearer s3cret-token"),
        "{}",
        heads[0]
    );
    drop(heads);
    // a failing call's error shows the command line: the token is not on it
    let (base, _) = serve_script(&[("404 Not Found", "")]);
    let err = GitHub::at(&base, &base, "curl")
        .with_token("s3cret-token")
        .api("x", false)
        .unwrap_err()
        .to_string();
    assert!(!err.contains("s3cret-token"), "{err}");
}

#[test]
fn no_token_sends_no_authorization_header() {
    let (base, seen) = serve_script(&[("200 OK", "{}")]);
    GitHub::at(&base, &base, "curl")
        .api("repos/f/m", false)
        .unwrap();
    assert!(
        !seen.lock().unwrap()[0]
            .to_ascii_lowercase()
            .contains("authorization"),
    );
}

#[test]
fn a_rate_limited_call_is_tried_again_and_succeeds() {
    let (base, seen) = serve_script(&[
        (
            "403 Forbidden\r\nRetry-After: 0",
            "{\"message\":\"slow down\"}",
        ),
        ("429 Too Many Requests\r\nRetry-After: 0", ""),
        ("200 OK", "{\"sha\":\"abc\"}"),
    ]);
    let gh = GitHub::at(&base, &base, "curl");
    assert_eq!(
        gh.api("repos/f/m/commits/v1", false).unwrap(),
        "{\"sha\":\"abc\"}"
    );
    assert_eq!(seen.lock().unwrap().len(), 3);
}

#[test]
fn a_call_that_stays_limited_fails_with_the_rate_limit_headers() {
    let (base, seen) = serve_script(&[LIMITED]);
    let anonymous = GitHub::at(&base, &base, "curl")
        .api("repos/f/m", false)
        .unwrap_err()
        .to_string();
    assert_eq!(seen.lock().unwrap().len(), 4);
    for part in [
        "HTTP 403",
        "4 tries",
        "x-ratelimit-limit: 60",
        "x-ratelimit-remaining: 0",
        "x-ratelimit-reset: 1700000000",
        "API rate limit exceeded",
        "no token was sent: set GH_TOKEN",
    ] {
        assert!(anonymous.contains(part), "{part} missing from {anonymous}");
    }
    let (base, _) = serve_script(&[LIMITED]);
    let with_token = GitHub::at(&base, &base, "curl")
        .with_token("t")
        .api("repos/f/m", false)
        .unwrap_err()
        .to_string();
    assert!(with_token.contains("the token was sent"), "{with_token}");
}

#[test]
fn other_error_statuses_are_not_retried() {
    let (base, seen) = serve_script(&[("404 Not Found", "{\"message\":\"Not Found\"}")]);
    let err = GitHub::at(&base, &base, "curl")
        .api("repos/f/none", false)
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("HTTP 404") && err.contains("Not Found"),
        "{err}"
    );
    assert!(!err.contains("tries"), "{err}");
    assert_eq!(seen.lock().unwrap().len(), 1);
}

#[test]
fn a_redirect_before_the_answer_is_followed() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let to = base.clone();
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let mut buf = [0_u8; 4096];
            let n = stream.read(&mut buf).unwrap_or(0);
            let head = String::from_utf8_lossy(&buf[..n]).into_owned();
            let reply = if head.contains("GET /moved ") {
                format!(
                    "HTTP/1.1 301 Moved\r\nLocation: {to}/there\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                )
            } else {
                "HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\ndone".to_owned()
            };
            let _ = stream.write_all(reply.as_bytes());
        }
    });
    assert_eq!(
        GitHub::at(&base, &base, "curl")
            .api("moved", false)
            .unwrap(),
        "done"
    );
}

#[test]
fn ci_without_a_token_fails_before_any_request_and_names_the_variable() {
    let (base, seen) = serve_script(&[("200 OK", "{}")]);
    let err = GitHub::at(&base, &base, "curl")
        .in_ci(true)
        .api("repos/f/m", false)
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("GH_TOKEN") && err.contains("github.token"),
        "{err}"
    );
    assert!(seen.lock().unwrap().is_empty());
    // with a token, CI runs the call; release downloads never need one
    GitHub::at(&base, &base, "curl")
        .in_ci(true)
        .with_token("t")
        .api("repos/f/m", false)
        .unwrap();
}

#[test]
fn the_wait_follows_retry_after_then_the_reset_then_the_backoff() {
    let wait = |text: &str| Reply::parse(text).wait(Duration::from_secs(7));
    assert_eq!(
        wait("HTTP/1.1 429 X\r\nRetry-After: 3\r\n\r\n"),
        Duration::from_secs(3)
    );
    assert_eq!(
        wait("HTTP/1.1 403 X\r\nRetry-After: 9999\r\n\r\n"),
        MAX_WAIT
    );
    assert_eq!(wait("HTTP/1.1 403 X\r\n\r\n"), Duration::from_secs(7));
    // the budget is spent and the reset is in the past: no wait
    assert_eq!(
        wait("HTTP/1.1 403 X\r\nx-ratelimit-remaining: 0\r\nx-ratelimit-reset: 1\r\n\r\n"),
        Duration::ZERO
    );
    // the budget is not spent (a permission 403): the backoff
    assert_eq!(
        wait("HTTP/1.1 403 X\r\nx-ratelimit-remaining: 5\r\nx-ratelimit-reset: 1\r\n\r\n"),
        Duration::from_secs(7)
    );
}
