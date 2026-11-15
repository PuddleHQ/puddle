// SPDX-License-Identifier: AGPL-3.0-or-later
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

use super::*;

const HOSTS: PlacementHosts = PlacementHosts {
    path: "path.test",
    query: "query.test",
    cargo: "cargo.test",
    go: "go.test",
};

fn ask(host: &str, target: &str, auth: Option<&str>) -> Option<WebAnswer> {
    let mut headers = vec![("host".to_owned(), host.to_owned())];
    if let Some(auth) = auth {
        headers.push(("authorization".to_owned(), auth.to_owned()));
    }
    placement_feeds(HOSTS, "real-token")(&Seen {
        method: "GET".into(),
        target: target.into(),
        headers,
        version: "HTTP/1.1",
        status: 200,
        body: Vec::new(),
    })
}

fn code(host: &str, target: &str, auth: Option<&str>) -> u16 {
    ask(host, target, auth).unwrap().status
}

#[test]
fn the_path_feed_wants_the_token_in_the_path_and_names_it_in_the_tarball_address() {
    assert_eq!(
        code("path.test", "/probe-private", Some("Bearer real-token")),
        401
    );
    assert_eq!(code("path.test", "/t/stand-in/probe-private", None), 401);
    assert_eq!(code("path.test", "/t/real-tokenx/probe-private", None), 401);
    let doc = ask("path.test", "/t/real-token/probe-private", None).unwrap();
    assert_eq!(doc.status, 200);
    let text = String::from_utf8(doc.body).unwrap();
    assert!(
        text.contains("https://path.test/t/real-token/probe-private/-/probe-private-1.0.0.tgz"),
        "{text}"
    );
    assert_eq!(
        code(
            "path.test",
            "/t/real-token/probe-private/-/probe-private-1.0.0.tgz",
            None
        ),
        200
    );
}

#[test]
fn the_query_feed_wants_the_token_in_its_one_parameter() {
    let file = format!("/files/{}", wheel_file());
    assert_eq!(code("query.test", &file, Some("Bearer real-token")), 401);
    assert_eq!(
        code("query.test", &format!("{file}?access_token=stand-in"), None),
        401
    );
    assert_eq!(
        code("query.test", &format!("{file}?token=real-token"), None),
        401
    );
    assert_eq!(
        code(
            "query.test",
            &format!("{file}?a=b&access_token=real-token"),
            None
        ),
        200
    );
}

#[test]
fn the_cargo_feed_takes_the_token_bare_and_serves_an_index_a_download_matches() {
    assert_eq!(code("cargo.test", "/config.json", None), 401);
    assert_eq!(code("cargo.test", "/config.json", Some("stand-in")), 401);
    assert_eq!(code("cargo.test", "/config.json", Some("real-token")), 200);
    let line = ask("cargo.test", "/pr/ob/probe-private", Some("real-token")).unwrap();
    let krate = ask(
        "cargo.test",
        "/dl/probe-private/1.0.0/download",
        Some("real-token"),
    )
    .unwrap();
    let doc: serde_json::Value = serde_json::from_slice(&line.body).unwrap();
    let sum = hex(&Sha256::digest(&krate.body));
    assert_eq!(doc["cksum"], sum);
    assert_eq!(code("cargo.test", "/pr/ob/other", Some("real-token")), 404);
}

#[test]
fn the_go_feed_takes_the_basic_password_and_serves_one_module() {
    let basic = format!("Basic {}", STANDARD.encode("user:real-token"));
    let wrong = format!("Basic {}", STANDARD.encode("user:stand-in"));
    for suffix in ["list", "v1.0.0.info", "v1.0.0.mod", "v1.0.0.zip"] {
        let target = format!("/example.com/probe/private/@v/{suffix}");
        assert_eq!(code("go.test", &target, None), 401);
        assert_eq!(code("go.test", &target, Some(&wrong)), 401);
        assert_eq!(code("go.test", &target, Some(&basic)), 200, "{suffix}");
    }
    assert_eq!(code("go.test", "/other/@v/list", Some(&basic)), 404);
}

#[test]
fn a_host_that_is_no_feed_is_left_alone() {
    assert!(ask("other.test", "/", None).is_none());
}
