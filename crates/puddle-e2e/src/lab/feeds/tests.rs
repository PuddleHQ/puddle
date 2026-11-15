// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

fn ask(feeds: &Responder, host: &str, target: &str, auth: Option<&str>) -> WebAnswer {
    let mut headers = vec![("host".to_owned(), host.to_owned())];
    if let Some(auth) = auth {
        headers.push(("authorization".to_owned(), auth.to_owned()));
    }
    feeds(&Seen {
        method: "GET".into(),
        target: target.into(),
        headers,
        version: "HTTP/1.1",
        status: 200,
        body: Vec::new(),
    })
    .unwrap()
}

fn feeds() -> Responder {
    private_feeds(
        "npm.test",
        "nuget.test",
        "pip.test",
        "mvn.test",
        "real-token",
    )
}

#[test]
fn a_feed_refuses_a_request_without_its_token_and_serves_one_with_it() {
    let feeds = feeds();
    let basic = format!("Basic {}", STANDARD.encode("user:real-token"));
    let wrong = format!("Basic {}", STANDARD.encode("user:stand-in"));
    for (host, path) in [
        ("npm.test", "/probe-private"),
        ("nuget.test", "/v3/index.json"),
        ("pip.test", "/simple/probe-private/"),
        (
            "mvn.test",
            "/m2/com/example/probe-private/1.0/probe-private-1.0.pom",
        ),
    ] {
        assert_eq!(ask(&feeds, host, path, None).status, 401, "{host}");
        assert_eq!(ask(&feeds, host, path, Some(&wrong)).status, 401, "{host}");
        let auth = if host == "npm.test" {
            "Bearer real-token"
        } else {
            &basic
        };
        assert_eq!(ask(&feeds, host, path, Some(auth)).status, 200, "{host}");
    }
}

#[test]
fn a_host_that_is_no_feed_is_left_alone_and_an_unknown_path_is_404() {
    let feeds = feeds();
    let seen = Seen {
        method: "GET".into(),
        target: "/".into(),
        headers: vec![("host".into(), "other.test".into())],
        version: "HTTP/1.1",
        status: 200,
        body: Vec::new(),
    };
    assert!(feeds(&seen).is_none());
    assert_eq!(
        ask(&feeds, "npm.test", "/nothing", Some("Bearer real-token")).status,
        404
    );
}

#[test]
fn the_npm_tarball_is_a_gzip_of_a_package() {
    let tarball = ask(
        &feeds(),
        "npm.test",
        "/probe-private/-/probe-private-1.0.0.tgz",
        Some("Bearer real-token"),
    );
    assert_eq!(&tarball.body[..2], &[0x1f, 0x8b]);
}

#[test]
fn every_feed_serves_its_package_and_nothing_else_to_a_request_with_the_token() {
    let feeds = feeds();
    let basic = format!("Basic {}", STANDARD.encode("user:real-token"));
    let body = |host: &str, path: &str| {
        let auth = if host == "npm.test" {
            "Bearer real-token"
        } else {
            &basic
        };
        let reply = ask(&feeds, host, path, Some(auth));
        (reply.status, reply.body)
    };
    let (status, npm) = body("npm.test", "/probe-private");
    assert_eq!(status, 200);
    assert!(
        String::from_utf8(npm)
            .unwrap()
            .contains("https://npm.test/probe-private/-/")
    );
    let (status, nuget) = body("nuget.test", "/v3/index.json");
    assert_eq!(status, 200);
    assert!(
        String::from_utf8(nuget)
            .unwrap()
            .contains("https://nuget.test/v3/flat/")
    );
    assert_eq!(
        body("nuget.test", "/v3/flat/probe.private/index.json").0,
        200
    );
    let (status, nupkg) = body(
        "nuget.test",
        "/v3/flat/probe.private/1.0.0/probe.private.1.0.0.nupkg",
    );
    assert_eq!((status, &nupkg[..2]), (200, &b"PK"[..]));
    assert_eq!(body("nuget.test", "/v3/flat/other/index.json").0, 404);
    let wheel_path = format!("/files/{}", wheel_name(PIP_PROJECT));
    assert_eq!(body("pip.test", "/simple/probe-private/").0, 200);
    assert_eq!(body("pip.test", &wheel_path).0, 200);
    assert_eq!(body("pip.test", "/simple/other/").0, 404);
    let base = "/m2/com/example/probe-private/1.0/probe-private-1.0";
    assert_eq!(body("mvn.test", &format!("{base}.pom")).0, 200);
    assert_eq!(body("mvn.test", &format!("{base}.jar")).0, 200);
    assert_eq!(body("mvn.test", &format!("{base}.jar.sha1")).0, 404);
}
