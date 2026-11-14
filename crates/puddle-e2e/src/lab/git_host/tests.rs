// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn options_name_every_certificate_name_and_the_validity() {
    let options = GitHostOptions::new("a.test", Versions::Http1)
        .also_for("b.test")
        .valid_between(2008, 2012);
    assert_eq!(options.certificate_names, ["a.test", "b.test"]);
    assert_eq!(options.valid, (2008, 2012));
    let options = options.certificate_for("c.test");
    assert_eq!(options.certificate_names, ["c.test"]);
}

#[test]
fn a_plain_request_is_held_by_its_authority_and_then_by_its_name() {
    assert_eq!(plain_authority("http://a.test/x?y"), "a.test:80");
    assert_eq!(plain_authority("http://a.test:8080?y"), "a.test:8080");
    assert_eq!(plain_authority("a.test:443"), "a.test:443");
    let (one, two): (SocketAddr, SocketAddr) = (
        "127.0.0.1:1".parse().unwrap(),
        "127.0.0.1:2".parse().unwrap(),
    );
    let names = vec![("a.test".to_owned(), one), ("a.test:80".to_owned(), two)];
    assert_eq!(held(&names, "a.test:80"), Some(two));
    assert_eq!(held(&names, "a.test:443"), Some(one));
    assert_eq!(held(&names, "b.test:80"), None);
}

#[test]
fn a_forwarded_head_loses_the_headers_meant_for_the_proxy() {
    let head = b"GET http://a.test/ HTTP/1.1\r\nHost: a.test\r\nProxy-Authorization: Negotiate abc\r\nproxy-connection: keep-alive\r\nAccept: */*\r\n\r\n";
    assert_eq!(
        String::from_utf8(without_proxy_headers(head)).unwrap(),
        "GET http://a.test/ HTTP/1.1\r\nHost: a.test\r\nAccept: */*\r\n\r\n"
    );
}

#[test]
fn a_path_is_read_as_owner_repository_and_the_rest() {
    assert_eq!(
        split_repo("/acme/web.git/info/refs"),
        Some(("acme/web".to_owned(), "/info/refs"))
    );
    for no in [
        "acme/web.git",
        "/acme",
        "/acme/web",
        "/acme/.git/x",
        "//web.git/x",
        "/a/b/c.git/x",
    ] {
        assert_eq!(split_repo(no), None, "{no}");
    }
}

#[test]
fn a_cgi_document_becomes_a_response() {
    let ok = cgi_response(b"Status: 404 Not Found\r\nContent-Type: text/plain\r\n\r\nnope");
    assert_eq!(ok.status().as_u16(), 404);
    assert_eq!(ok.headers().get("content-type").unwrap(), "text/plain");
    // A bare-newline document and a status that is no number.
    let odd = cgi_response(b"Status: abc\nX-A: 1\n\nbody");
    assert_eq!(odd.status().as_u16(), 200);
    assert_eq!(odd.headers().get("x-a").unwrap(), "1");
    assert_eq!(cgi_response(b"no separator").status().as_u16(), 500);
}

#[test]
fn an_lfs_operation_is_read_from_the_body_and_defaults_to_none() {
    assert_eq!(lfs_operation(br#"{"operation":"upload"}"#), "upload");
    assert_eq!(lfs_operation(b"not json"), "");
}

#[test]
fn names_under_test_and_invalid_are_known_not_to_resolve_and_others_are_not() {
    for reserved in [
        "a.test:80",
        "A.Test.:443",
        "x.corp.test:80",
        "nothing.invalid:1",
    ] {
        assert!(is_reserved_name(reserved), "{reserved}");
    }
    for ordinary in [
        "a.example:80",
        "contest:80",
        "latest.com:443",
        "127.0.0.1:80",
        "[::1]:80",
        "test:80",
    ] {
        assert!(!is_reserved_name(ordinary), "{ordinary}");
    }
}

#[tokio::test]
async fn a_tunnel_to_a_reserved_name_the_proxy_does_not_hold_is_refused_at_once() {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let proxy = NameProxy::start(&[]).await;
    let mut client = TcpStream::connect(("127.0.0.1", proxy.port()))
        .await
        .unwrap();
    client
        .write_all(b"CONNECT nobody-holds-this.test:80 HTTP/1.1\r\nHost: nobody-holds-this.test:80\r\n\r\n")
        .await
        .unwrap();
    let mut answer = String::new();
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        client.read_to_string(&mut answer),
    )
    .await
    .expect("the answer waited for a name lookup")
    .unwrap();
    assert!(answer.starts_with("HTTP/1.1 502"), "{answer}");
}
