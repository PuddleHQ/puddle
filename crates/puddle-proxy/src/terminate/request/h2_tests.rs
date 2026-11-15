// SPDX-License-Identifier: AGPL-3.0-or-later
#[test]
fn an_unbuildable_injected_header_is_a_bad_gateway() {
    assert_eq!(unbuildable_header().status, "502 Bad Gateway");
}

use puddle_types::Host;

use super::*;
use crate::terminate::inject::{InjectedHeader, SecretValue};

fn target() -> Target {
    Target {
        host: Host::parse_normalised("bound.test").unwrap(),
        port: 443,
    }
}

fn guest_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert("host", HeaderValue::from_static("bound.test"));
    headers.insert("authorization", HeaderValue::from_static("Bearer guest"));
    headers.insert("te", HeaderValue::from_static("trailers"));
    headers.insert("content-length", HeaderValue::from_static("3"));
    headers.insert("expect", HeaderValue::from_static("100-continue"));
    headers.insert("user-agent", HeaderValue::from_static("tool"));
    headers.append("cookie", HeaderValue::from_static("a=1"));
    headers.append("cookie", HeaderValue::from_static("b=2"));
    headers
}

fn injection() -> Injection {
    Injection::new(
        "binding-1",
        vec![InjectedHeader::new("authorization", SecretValue::new("Basic xyz")).unwrap()],
    )
}

#[test]
fn toward_an_http2_server_te_trailers_stays_and_crumbs_are_not_joined() {
    let map = upstream_headers_h2(&guest_headers(), &target(), None, UpstreamVersion::H2).unwrap();
    assert_eq!(map["te"], "trailers");
    assert!(map.get("host").is_none());
    assert!(map.get("content-length").is_none() && map.get("expect").is_none());
    assert_eq!(map.get_all("cookie").iter().count(), 2);
    assert_eq!(map["authorization"], "Bearer guest");
}

#[test]
fn toward_an_http11_server_te_goes_host_is_set_and_cookies_are_joined() {
    let map = upstream_headers_h2(&guest_headers(), &target(), None, UpstreamVersion::H1).unwrap();
    assert!(map.get("te").is_none());
    assert_eq!(map["host"], "bound.test");
    assert_eq!(map["cookie"], "a=1; b=2");
    assert_eq!(map["user-agent"], "tool");
}

#[test]
fn a_te_that_is_not_trailers_never_goes_on() {
    let mut headers = guest_headers();
    headers.insert("te", HeaderValue::from_static("gzip"));
    let map = upstream_headers_h2(&headers, &target(), None, UpstreamVersion::H2).unwrap();
    assert!(map.get("te").is_none());
}

#[test]
fn an_injected_credential_replaces_the_guests_and_is_sensitive() {
    for version in [UpstreamVersion::H1, UpstreamVersion::H2] {
        let map =
            upstream_headers_h2(&guest_headers(), &target(), Some(&injection()), version).unwrap();
        assert_eq!(map.get_all("authorization").iter().count(), 1);
        assert_eq!(map["authorization"], "Basic xyz");
        assert!(map["authorization"].is_sensitive());
    }
}

#[test]
fn an_injected_header_of_another_name_leaves_the_guests_authorization_alone() {
    let other = Injection::new(
        "binding-1",
        vec![InjectedHeader::new("x-extra", SecretValue::new("injected")).unwrap()],
    );
    for version in [UpstreamVersion::H1, UpstreamVersion::H2] {
        let map = upstream_headers_h2(&guest_headers(), &target(), Some(&other), version).unwrap();
        assert_eq!(map["authorization"], "Bearer guest");
        assert_eq!(map["x-extra"], "injected");
    }
}

#[test]
fn a_port_other_than_443_is_part_of_the_host_header() {
    let other = Target {
        host: Host::parse_normalised("bound.test").unwrap(),
        port: 8443,
    };
    let map = upstream_headers_h2(&HeaderMap::new(), &other, None, UpstreamVersion::H1).unwrap();
    assert_eq!(map["host"], "bound.test:8443");
}
