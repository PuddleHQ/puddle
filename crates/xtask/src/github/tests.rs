// SPDX-License-Identifier: GPL-3.0-or-later
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
    let gh = GitHub::at(
        "http://127.0.0.1:1",
        "http://127.0.0.1:1",
        "puddle-no-such-curl",
    );
    let dir = tempfile::tempdir().unwrap();
    for err in [
        gh.api("repos/f/m/commits/v1", false)
            .unwrap_err()
            .to_string(),
        gh.release_asset("f/m", "v1", "a", dir.path())
            .unwrap_err()
            .to_string(),
    ] {
        assert!(
            err.contains("`puddle-no-such-curl` is not installed"),
            "{err}"
        );
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
