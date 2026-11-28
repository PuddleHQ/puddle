// SPDX-License-Identifier: AGPL-3.0-or-later
//! I tests of a refused `CONNECT` answered inside the tunnel: libcurl drops the reason and body
//! of a non-2xx `CONNECT`, so a tool that trusts the workspace's CA gets the refusal as the
//! answer to its first HTTPS request instead.
//!
//! The real-tool test needs `curl` on the path; without it the test skips unless
//! `PUDDLE_REQUIRE_TOOLS` is set (CI sets it). It is Unix-only, like the other real-tool tests
//! (`proxy_port` is).
use crate::terminate_support;

use puddle_types::ConnectionDecision;
use terminate_support::{Pki, RigBuilder};

#[tokio::test]
async fn a_refused_host_is_told_to_a_tool_inside_the_tunnel() {
    let pki = Pki::new();
    let rig = RigBuilder::new(&pki).build();
    let mut guest = rig.guest();
    let mut client = guest.tls("stranger.test:443", None).await.unwrap();
    let response = client.get("stranger.test", "/index/config.json").await;
    assert_eq!(response.status, 403);
    assert!(
        response.text().contains("stranger.test"),
        "the body names the host: {}",
        response.text()
    );
    assert_eq!(response.header("x-puddle-decision"), Some("pending"));
    let events = rig.events(1).await;
    assert_eq!(events[0].decision, ConnectionDecision::Pending);
}

#[tokio::test]
async fn a_refusal_is_not_told_to_a_client_with_another_name_or_only_h2() {
    let pki = Pki::new();
    let rig = RigBuilder::new(&pki).build();
    for (sni, alpn) in [
        (Some("other.test"), &[&b"http/1.1"[..]][..]),
        (None, &[&b"h2"[..]][..]),
    ] {
        let mut guest = rig.guest();
        let err = guest
            .tls_with("stranger.test:443", sni, alpn, true)
            .await
            .err()
            .unwrap_or_else(|| panic!("sni {sni:?} alpn {alpn:?} was answered"));
        assert!(
            matches!(
                err.kind(),
                std::io::ErrorKind::UnexpectedEof | std::io::ErrorKind::ConnectionReset
            ),
            "the connection is closed without an alert or a certificate: {err:?}"
        );
    }
}

#[tokio::test]
async fn a_client_in_a_container_gets_the_plain_refusal() {
    let pki = Pki::new();
    let rig = RigBuilder::new(&pki).build();
    let mut guest = rig.guest().in_container();
    let (status, _) = guest.connect_to("stranger.test:443").await;
    assert_eq!(status, 403);
}

#[tokio::test]
async fn a_refusal_on_another_port_is_the_plain_connect_answer() {
    let pki = Pki::new();
    let rig = RigBuilder::new(&pki).build();
    let mut guest = rig.guest();
    let (status, _) = guest.connect_to("stranger.test:8443").await;
    assert_eq!(status, 403);
}

#[cfg(unix)]
#[tokio::test]
async fn curl_prints_the_refusal_text_instead_of_a_bare_tunnel_error() {
    use std::process::Command;

    if Command::new("curl").arg("--version").output().is_err() {
        assert!(
            std::env::var_os("PUDDLE_REQUIRE_TOOLS").is_none(),
            "PUDDLE_REQUIRE_TOOLS is set but curl is not on the path"
        );
        eprintln!("skipped: curl not found");
        return;
    }
    let pki = Pki::new();
    let rig = RigBuilder::new(&pki).build();
    let guest = rig.guest();
    let (port, _proxy) = guest.proxy_port().await;
    let dir = tempfile::tempdir().unwrap();
    let ca = dir.path().join("ca.pem");
    std::fs::write(&ca, rig.ca.certificate().pem()).unwrap();
    let output = tokio::task::spawn_blocking(move || {
        Command::new("curl")
            .args(["--silent", "--show-error", "--max-time", "20", "--proxy"])
            .arg(format!("http://127.0.0.1:{port}"))
            .arg("--cacert")
            .arg(ca)
            .arg("https://stranger.test/index/config.json")
            .output()
            .unwrap()
    })
    .await
    .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert!(
        stdout.contains("stranger.test is waiting for approval in Puddle"),
        "{stdout}\n{stderr}"
    );
}

/// Real cargo, sparse registry on a pending host: cargo must print the refusal text and end
/// at once. Before the answer moved inside the tunnel cargo saw a bare tunnel failure and
/// retried 79 times over 3 min 36 s.
#[cfg(unix)]
#[tokio::test]
async fn cargo_prints_the_refusal_text_and_does_not_retry() {
    use std::process::Command;

    if Command::new("cargo").arg("--version").output().is_err() {
        assert!(
            std::env::var_os("PUDDLE_REQUIRE_TOOLS").is_none(),
            "PUDDLE_REQUIRE_TOOLS is set but cargo is not on the path"
        );
        eprintln!("skipped: cargo not found");
        return;
    }
    let pki = Pki::new();
    let rig = RigBuilder::new(&pki).build();
    let guest = rig.guest();
    let (port, _proxy) = guest.proxy_port().await;
    let dir = tempfile::tempdir().unwrap();
    let ca = dir.path().join("ca.pem");
    std::fs::write(&ca, rig.ca.certificate().pem()).unwrap();
    let project = dir.path().join("app");
    std::fs::create_dir_all(project.join("src")).unwrap();
    std::fs::create_dir_all(project.join(".cargo")).unwrap();
    std::fs::write(project.join("src/main.rs"), "fn main() {}\n").unwrap();
    std::fs::write(
        project.join("Cargo.toml"),
        "[workspace]\n\n[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nserde = \"1\"\n",
    )
    .unwrap();
    std::fs::write(
        project.join(".cargo/config.toml"),
        "[source.crates-io]\nreplace-with = \"mirror\"\n\n[source.mirror]\nregistry = \"sparse+https://stranger.test/index/\"\n",
    )
    .unwrap();
    let home = dir.path().join("cargo-home");
    let output = tokio::task::spawn_blocking(move || {
        Command::new("cargo")
            .args(["fetch", "--color", "never"])
            .current_dir(&project)
            .env("CARGO_HOME", home)
            .env("CARGO_HTTP_PROXY", format!("http://127.0.0.1:{port}"))
            .env("CARGO_HTTP_CAINFO", ca)
            .env("CARGO_NET_RETRY", "2")
            .env("CARGO_HTTP_TIMEOUT", "20")
            .output()
            .unwrap()
    })
    .await
    .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{stderr}");
    assert!(
        stderr.contains("stranger.test is waiting for approval in Puddle"),
        "{stderr}"
    );
    assert!(!stderr.contains("spurious"), "cargo retried: {stderr}");
}

/// A client that pins a key (or trusts another CA) cannot read the refusal: it fails the TLS
/// handshake with a certificate error, and the proxy never reaches the real server.
#[cfg(unix)]
#[tokio::test]
async fn a_pinned_client_fails_the_handshake_instead_of_reading_the_refusal() {
    use std::process::Command;

    if Command::new("curl").arg("--version").output().is_err() {
        assert!(
            std::env::var_os("PUDDLE_REQUIRE_TOOLS").is_none(),
            "PUDDLE_REQUIRE_TOOLS is set but curl is not on the path"
        );
        eprintln!("skipped: curl not found");
        return;
    }
    let pki = Pki::new();
    let rig = RigBuilder::new(&pki).build();
    let guest = rig.guest();
    let (port, _proxy) = guest.proxy_port().await;
    let dir = tempfile::tempdir().unwrap();
    let ca = dir.path().join("ca.pem");
    std::fs::write(&ca, rig.ca.certificate().pem()).unwrap();
    let output = tokio::task::spawn_blocking(move || {
        Command::new("curl")
            .args(["--silent", "--show-error", "--max-time", "20", "--proxy"])
            .arg(format!("http://127.0.0.1:{port}"))
            .arg("--cacert")
            .arg(ca)
            .args([
                "--pinnedpubkey",
                "sha256//AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
            ])
            .arg("https://stranger.test/index/config.json")
            .output()
            .unwrap()
    })
    .await
    .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{stdout}\n{stderr}");
    assert!(!stdout.contains("waiting for approval"), "{stdout}");
}
