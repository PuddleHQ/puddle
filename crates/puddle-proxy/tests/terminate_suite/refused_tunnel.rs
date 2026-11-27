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
    assert!(response.header("x-puddle-decision").is_some());
    let events = rig.events(1).await;
    assert_eq!(events[0].decision, ConnectionDecision::Pending);
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
