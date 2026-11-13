// SPDX-License-Identifier: AGPL-3.0-or-later
use puddle_types::Host;

use super::*;

fn target(host: &str, port: u16) -> Target {
    Target {
        host: Host::parse_normalised(host).unwrap(),
        port,
    }
}

fn head(request: &str) -> Head {
    let mut lines = request.split("\r\n");
    let first = lines.next().unwrap();
    let mut parts = first.split(' ');
    Head {
        method: parts.next().unwrap().into(),
        uri: parts.next().unwrap().into(),
        version: parts.next().unwrap().into(),
        headers: lines.filter(|l| !l.is_empty()).map(Into::into).collect(),
    }
}

fn status(r: &Refusal) -> &str {
    r.status.split(' ').next().unwrap()
}

#[test]
fn a_request_for_the_connection_host_passes() {
    let t = target("github.com", 443);
    for req in [
        "GET /a/b?c=d HTTP/1.1\r\nHost: github.com\r\n\r\n",
        "GET /a HTTP/1.1\r\nHost: GitHub.com:443\r\n\r\n",
        "GET https://github.com/a HTTP/1.1\r\nHost: github.com\r\n\r\n",
        "GET https://GITHUB.com:443?x=1 HTTP/1.1\r\nHost: github.com\r\n\r\n",
    ] {
        let parsed = parse(&head(req), &t).unwrap_or_else(|r| panic!("{req}: {r:?}"));
        assert!(parsed.target.starts_with('/'), "{req}");
    }
    assert_eq!(
        parse(
            &head("GET https://github.com?x=1 HTTP/1.1\r\nHost: github.com\r\n\r\n"),
            &t
        )
        .unwrap()
        .target,
        "/?x=1"
    );
}

#[test]
fn another_host_is_421_and_malformed_is_400() {
    let t = target("github.com", 443);
    for req in [
        "GET /a HTTP/1.1\r\nHost: evil.example\r\n\r\n",
        "GET /a HTTP/1.1\r\nHost: github.com:8443\r\n\r\n",
        "GET /a HTTP/1.1\r\nHost: 140.82.112.3\r\n\r\n",
        "GET https://evil.example/a HTTP/1.1\r\nHost: github.com\r\n\r\n",
        "GET https://github.com:444/a HTTP/1.1\r\nHost: github.com\r\n\r\n",
    ] {
        assert_eq!(status(&parse(&head(req), &t).unwrap_err()), "421", "{req}");
    }
    for req in [
        "GET /a HTTP/1.1\r\n\r\n",
        "GET /a HTTP/1.1\r\nHost: github.com\r\nHost: github.com\r\n\r\n",
        "GET /a HTTP/1.1\r\nHost: github.com\r\nhost: evil.example\r\n\r\n",
        "GET /a#frag HTTP/1.1\r\nHost: github.com\r\n\r\n",
        "GET a HTTP/1.1\r\nHost: github.com\r\n\r\n",
        "GET http://github.com/a HTTP/1.1\r\nHost: github.com\r\n\r\n",
        "GET https://user@github.com/a HTTP/1.1\r\nHost: github.com\r\n\r\n",
        "CONNECT evil.example:443 HTTP/1.1\r\nHost: evil.example\r\n\r\n",
        "GET /a HTTP/1.1\r\nHost: github.com\r\nContent-Length: 1\r\nTransfer-Encoding: chunked\r\n\r\n",
        "GET /a HTTP/1.1\r\nHost: github.com\r\nTransfer-Encoding: gzip\r\n\r\n",
        "GET /a HTTP/1.1\r\nHost: github.com\r\nContent-Length: 1\r\nContent-Length: 2\r\n\r\n",
        "GET /a HTTP/1.0\r\nHost: github.com\r\nTransfer-Encoding: chunked\r\n\r\n",
    ] {
        assert_eq!(status(&parse(&head(req), &t).unwrap_err()), "400", "{req}");
    }
    assert_eq!(
            status(
                &parse(
                    &head("POST /a HTTP/1.1\r\nHost: github.com\r\nExpect: 200-ok\r\nContent-Length: 1\r\n\r\n"),
                    &t
                )
                .unwrap_err()
            ),
            "417"
        );
}

#[test]
fn flags_come_from_the_headers() {
    let t = target("github.com", 443);
    let p = parse(
            &head("POST /a HTTP/1.1\r\nHost: github.com\r\nExpect: 100-continue\r\nContent-Length: 4\r\n\r\n"),
            &t,
        )
        .unwrap();
    assert!(p.expect_continue && !p.close && p.http11);
    assert_eq!(p.body, Body::Length(4));
    let p = parse(
        &head("GET /a HTTP/1.1\r\nHost: github.com\r\nConnection: Keep-Alive, close\r\n\r\n"),
        &t,
    )
    .unwrap();
    assert!(p.close);
    let p = parse(&head("GET /a HTTP/1.0\r\n\r\n"), &t).unwrap();
    assert!(p.close && !p.http11);
}

#[test]
fn a_hundred_continue_is_expected_only_by_an_http11_request_with_a_body() {
    let t = target("github.com", 443);
    let no_body = parse(
        &head("GET /a HTTP/1.1\r\nHost: github.com\r\nExpect: 100-continue\r\n\r\n"),
        &t,
    )
    .unwrap();
    assert!(!no_body.expect_continue, "no body, nothing to continue");
    let old = parse(
            &head("POST /a HTTP/1.0\r\nHost: github.com\r\nExpect: 100-continue\r\nContent-Length: 4\r\n\r\n"),
            &t,
        )
        .unwrap();
    assert!(!old.expect_continue, "HTTP/1.0 has no 100 Continue");
}

#[test]
fn a_websocket_upgrade_needs_http11_get_and_no_body() {
    let t = target("github.com", 443);
    let upgrade = |line: &str, extra: &str| {
        parse(
                &head(&format!(
                    "{line}\r\nHost: github.com\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n{extra}\r\n"
                )),
                &t,
            )
            .unwrap()
            .upgrade
    };
    assert!(upgrade("GET /a HTTP/1.1", ""));
    assert!(!upgrade("GET /a HTTP/1.0", ""), "HTTP/1.0 has no upgrade");
    assert!(!upgrade("POST /a HTTP/1.1", ""), "only GET upgrades");
    assert!(
        !upgrade("GET /a HTTP/1.1", "Content-Length: 2\r\n"),
        "a request with a body does not"
    );
}

#[test]
fn upstream_headers_are_rebuilt() {
    let t = target("github.com", 443);
    let h = head(
        "POST /a HTTP/1.1\r\nHost: github.com\r\nConnection: x-hop, keep-alive\r\nX-Hop: 1\r\nAuthorization: Bearer guest\r\nProxy-Authorization: Basic zz\r\nContent-Length: 4\r\nTE: trailers\r\nUpgrade: websocket\r\nAccept: */*\r\nAccept: text/plain\r\n\r\n",
    );
    let plain = upstream_headers(&h, &t, None).unwrap();
    let names: Vec<_> = plain.keys().map(HeaderName::as_str).collect();
    assert_eq!(names, ["host", "authorization", "accept"]);
    assert_eq!(plain.get_all("accept").iter().count(), 2);
    assert_eq!(plain["host"], "github.com");
    let other = target("github.com", 8443);
    assert_eq!(
        upstream_headers(&h, &other, None).unwrap()["host"],
        "github.com:8443"
    );
}

#[test]
fn injection_replaces_the_guests_credentials() {
    use super::super::inject::{InjectedHeader, SecretValue};
    let t = target("github.com", 443);
    let h = head(
        "GET /a HTTP/1.1\r\nHost: github.com\r\nAuthorization: Bearer guest\r\nX-Extra: guest\r\n\r\n",
    );
    let injection = Injection::new(
        "b1",
        vec![
            InjectedHeader::new("Authorization", SecretValue::new("Basic abc")).unwrap(),
            InjectedHeader::new("x-extra", SecretValue::new("injected")).unwrap(),
        ],
    );
    let map = upstream_headers(&h, &t, Some(&injection)).unwrap();
    assert_eq!(map.get_all("authorization").iter().count(), 1);
    assert_eq!(map["authorization"], "Basic abc");
    assert!(map["authorization"].is_sensitive());
    assert_eq!(map.get_all("x-extra").iter().count(), 1);
    assert_eq!(map["x-extra"], "injected");
    assert!(!format!("{injection:?}").contains("abc"));
}

#[test]
fn an_injected_header_of_another_name_leaves_the_guests_authorization_alone() {
    use super::super::inject::{InjectedHeader, SecretValue};
    let t = target("github.com", 443);
    let h = head("GET /a HTTP/1.1\r\nHost: github.com\r\nAuthorization: Bearer guest\r\n\r\n");
    let injection = Injection::new(
        "b1",
        vec![InjectedHeader::new("x-extra", SecretValue::new("injected")).unwrap()],
    );
    let map = upstream_headers(&h, &t, Some(&injection)).unwrap();
    assert_eq!(map["authorization"], "Bearer guest");
    assert_eq!(map["x-extra"], "injected");
}

#[test]
fn injectors_cannot_set_proxy_owned_headers() {
    use super::super::inject::{HeaderError, InjectedHeader, SecretValue};
    for name in [
        "Host",
        "content-length",
        "Transfer-Encoding",
        "connection",
        "proxy-authorization",
        "Upgrade",
    ] {
        assert_eq!(
            InjectedHeader::new(name, SecretValue::new("x")).unwrap_err(),
            HeaderError::Name,
            "{name}"
        );
    }
    assert_eq!(
        InjectedHeader::new("authorization", SecretValue::new("a\r\nb: c")).unwrap_err(),
        HeaderError::Value
    );
    assert_eq!(
        InjectedHeader::new("bad name", SecretValue::new("x")).unwrap_err(),
        HeaderError::Name
    );
}
