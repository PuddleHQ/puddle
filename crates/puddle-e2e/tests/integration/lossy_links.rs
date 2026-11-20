// SPDX-License-Identifier: AGPL-3.0-or-later
//! Downloads through the real guest agent and proxy while the link behind the proxy misbehaves
//! (QA playbook 07, S18 and S19). The link is a userspace shim in front of a plain origin (see
//! `rig/link_shim.rs`), so no root and no namespace are needed.
//!
//! The bars: an upstream that pauses for 20 s in the middle of a download and then goes on
//! completes the download byte for byte; one that resets in the middle gives the client an
//! aborted transfer (an error, never an end of file that would make a cut-off file look whole)
//! and the next tunnel works; and `npm ci` over a slow, lossy link succeeds with no 502 or 504
//! that Puddle made.
//!
//! The npm case needs `npm` (it ships with the `node` 24 that `node_fetch.rs` needs); without it
//! the case says so and passes locally, and with `PUDDLE_REQUIRE_NODE` set it fails instead.
#![cfg(target_os = "linux")]
#![expect(
    clippy::unwrap_used,
    reason = "helpers outside #[test] functions fail the test by panicking"
)]

use std::net::{IpAddr, SocketAddr};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use flate2::Compression;
use flate2::write::GzEncoder;
use puddle_e2e::lab::{WebAnswer, WebHost, WebOptions};
use puddle_proxy::testing::StaticResolver;
use puddle_upstream::{Config as DiscoveryConfig, Discovery, FakeOs, Hop, ProxyConfig};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::time::timeout;

use crate::rig::{LOCAL, LinkShim, Rig, allow, basic, rig_with, try_tunnel};

const NAME: &str = "lab.test";

/// The bytes of the big download: a pattern no truncation can mimic.
const SIZE: usize = 1024 * 1024;

fn pattern() -> Vec<u8> {
    #[expect(clippy::cast_possible_truncation, reason = "a repeating byte pattern")]
    (0..SIZE).map(|i| (i.wrapping_mul(31) >> 3) as u8).collect()
}

fn sha(bytes: &[u8]) -> Vec<u8> {
    Sha256::digest(bytes).to_vec()
}

/// An origin that answers `GET /sized` with a `content-length` and `GET /open` with a body that
/// ends only when the connection does (what an old server or a streamed export does).
async fn origin() -> SocketAddr {
    let listener = TcpListener::bind((LOCAL, 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut conn, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut head = Vec::new();
                let mut byte = [0_u8; 1];
                while !head.ends_with(b"\r\n\r\n") {
                    if conn.read_exact(&mut byte).await.is_err() {
                        return;
                    }
                    head.push(byte[0]);
                }
                let head = String::from_utf8_lossy(&head);
                let reply = if head.starts_with("GET /sized ") {
                    format!(
                        "HTTP/1.1 200 OK\r\ncontent-length: {SIZE}\r\nconnection: close\r\n\r\n"
                    )
                } else {
                    "HTTP/1.1 200 OK\r\nconnection: close\r\n\r\n".to_owned()
                };
                let _ = conn.write_all(reply.as_bytes()).await;
                let _ = conn.write_all(&pattern()).await;
                let _ = conn.shutdown().await;
            });
        }
    });
    addr
}

/// The proxy and agent over a direct route, with `NAME` allowed and resolving to this machine.
async fn rig() -> Rig {
    let os = FakeOs::new(ProxyConfig {
        pac_url: Some("http://pac.corp/proxy.pac".into()),
        ..ProxyConfig::default()
    });
    os.set_pac(|_| Ok(vec![Hop::Direct]));
    let discovery = Discovery::new(os, DiscoveryConfig::default());
    let resolver = StaticResolver::new().with(NAME, &[IpAddr::V4(LOCAL)]);
    let rig = rig_with(discovery, basic("lab", "lab"), resolver).await;
    allow(&rig.store, NAME);
    rig
}

/// What a client read of one `GET` through a tunnel to the link: the body bytes, and how the
/// read ended (`Ok` is an end of file, `Err` an abort).
async fn get(rig: &Rig, link: &LinkShim, path: &str) -> (Vec<u8>, std::io::Result<()>) {
    let authority = format!("{NAME}:{}", link.addr().port());
    let mut tunnel = try_tunnel(rig.agent.local_addr(), &authority)
        .await
        .unwrap();
    tunnel
        .write_all(
            format!("GET {path} HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .await
        .unwrap();
    let mut all = Vec::new();
    let mut buf = vec![0_u8; 32 * 1024];
    let end = loop {
        match tunnel.read(&mut buf).await {
            Ok(0) => break Ok(()),
            Ok(n) => all.extend_from_slice(buf.get(..n).unwrap_or_default()),
            Err(err) => break Err(err),
        }
    };
    let split = all
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map_or(all.len(), |at| at + 4);
    (all.split_off(split), end)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_upstream_that_pauses_for_20_seconds_mid_download_completes_byte_for_byte() {
    let link = LinkShim::start(origin().await);
    let rig = rig().await;
    link.pause_once(512 * 1024, Duration::from_secs(20));
    let started = Instant::now();
    let (body, end) = timeout(Duration::from_secs(120), get(&rig, &link, "/sized"))
        .await
        .expect("the download did not finish");
    let took = started.elapsed();
    end.unwrap();
    assert_eq!(body.len(), SIZE, "short download after {took:?}");
    assert_eq!(sha(&body), sha(&pattern()), "the bytes differ");
    // The pause really happened: without it this would be a download of a megabyte over loopback.
    assert!(
        took >= Duration::from_secs(20),
        "the link did not pause: {took:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_upstream_reset_mid_download_is_an_aborted_transfer_for_the_client_and_the_next_tunnel_works()
 {
    let link = LinkShim::start(origin().await);
    let rig = rig().await;
    link.reset_once(256 * 1024);
    let (body, end) = timeout(Duration::from_secs(60), get(&rig, &link, "/open"))
        .await
        .expect("the client was left waiting after the reset");
    assert!(
        body.len() < SIZE,
        "the whole body came down before the reset: {}",
        body.len()
    );
    // The body has no length: a clean end of file here would make a cut-off file look whole.
    let err = end.expect_err("the client read a clean end of file after an upstream reset");
    eprintln!("the client's error: {err}");

    // The reset was a one-shot: the next tunnel through the same agent and proxy carries a whole file.
    let (body, end) = timeout(Duration::from_secs(60), get(&rig, &link, "/open"))
        .await
        .expect("the next download did not finish");
    end.unwrap();
    assert_eq!(
        sha(&body),
        sha(&pattern()),
        "the next tunnel's bytes differ"
    );
    assert!(
        link.carried().connections >= 2,
        "the next download did not use a new connection through the link"
    );
}

/// A gzip tarball of the npm package `probe-big`, about a megabyte of bytes that do not compress.
fn big_package() -> Vec<u8> {
    let mut noise = vec![0_u8; SIZE];
    let mut state = 0x9e37_79b9_u32;
    for byte in &mut noise {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        *byte = state.to_le_bytes()[0];
    }
    let mut archive = tar::Builder::new(Vec::new());
    for (name, data) in [
        (
            "package/package.json",
            br#"{"name":"probe-big","version":"1.0.0"}"#.to_vec(),
        ),
        ("package/noise.bin", noise),
    ] {
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_mtime(0);
        archive.append_data(&mut header, name, &data[..]).unwrap();
    }
    let raw = archive.into_inner().unwrap();
    let mut gz = GzEncoder::new(Vec::new(), Compression::fast());
    std::io::Write::write_all(&mut gz, &raw).unwrap();
    gz.finish().unwrap()
}

/// Whether `npm` can run here; panics when `PUDDLE_REQUIRE_NODE` is set and it cannot.
#[expect(
    clippy::print_stderr,
    reason = "a local run without npm says that it skipped"
)]
fn npm_available() -> bool {
    let ok = Command::new("npm")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    assert!(
        ok || std::env::var_os("PUDDLE_REQUIRE_NODE").is_none(),
        "PUDDLE_REQUIRE_NODE is set but npm is not on PATH"
    );
    if !ok {
        eprintln!("skipped: npm not on PATH");
    }
    ok
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn npm_ci_over_a_slow_lossy_link_succeeds_with_no_502_or_504_from_puddle() {
    if !npm_available() {
        return;
    }
    let tarball = big_package();
    let served = tarball.clone();
    let registry = WebHost::start(
        &[NAME],
        WebOptions {
            responder: Some(Arc::new(move |seen| {
                let path = seen.target.split('?').next().unwrap_or_default();
                (path == "/probe-big/-/probe-big-1.0.0.tgz").then(|| WebAnswer {
                    status: 200,
                    content_type: "application/octet-stream",
                    body: served.clone(),
                })
            })),
            ..WebOptions::default()
        },
    )
    .await;
    let link = LinkShim::start(registry.plain_addr());
    // The numbers of the playbook: 300 ms of delay on every chunk and one chunk in ten held for a
    // retransmission timeout (a loss shows to the program above TCP as exactly that).
    link.lossy(Duration::from_millis(300), 10, Duration::from_secs(1));
    let rig = rig().await;

    let project = tempfile::tempdir().unwrap();
    let port = link.addr().port();
    std::fs::write(
        project.path().join("package.json"),
        r#"{"name":"app","version":"1.0.0","dependencies":{"probe-big":"1.0.0"}}"#,
    )
    .unwrap();
    std::fs::write(
        project.path().join("package-lock.json"),
        serde_json::json!({
            "name": "app", "version": "1.0.0", "lockfileVersion": 3, "requires": true,
            "packages": {
                "": {"name": "app", "version": "1.0.0", "dependencies": {"probe-big": "1.0.0"}},
                "node_modules/probe-big": {
                    "version": "1.0.0",
                    "resolved": format!("http://{NAME}:{port}/probe-big/-/probe-big-1.0.0.tgz"),
                },
            },
        })
        .to_string(),
    )
    .unwrap();
    let agent = format!("http://{}", rig.agent.local_addr());
    let dir = project.path().to_owned();
    let home = tempfile::tempdir().unwrap();
    let home_dir = home.path().to_owned();
    let out = tokio::task::spawn_blocking(move || {
        Command::new("npm")
            .args([
                "ci",
                "--ignore-scripts",
                "--no-audit",
                "--no-fund",
                "--loglevel=http",
            ])
            .current_dir(&dir)
            .env("HOME", &home_dir)
            .env("npm_config_cache", home_dir.join("cache"))
            .env("npm_config_proxy", &agent)
            .env("npm_config_fetch_retries", "0")
            .env_remove("NO_PROXY")
            .env_remove("no_proxy")
            .output()
            .unwrap()
    })
    .await
    .unwrap();
    let log = format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "npm ci failed:\n{log}");
    assert!(
        project
            .path()
            .join("node_modules/probe-big/noise.bin")
            .is_file(),
        "{log}"
    );
    for code in [" 502 ", " 504 ", "Bad Gateway", "Gateway Timeout"] {
        assert!(
            !log.contains(code),
            "a Puddle-made error ({code:?}) in npm's log:\n{log}"
        );
    }
    // The link did what the case says: the package came through it, and chunks were held.
    let carried = link.carried();
    assert!(
        carried.bytes_down >= tarball.len() as u64,
        "the package did not come through the link: {carried:?}"
    );
    assert!(
        carried.stalled_chunks >= 1,
        "no chunk was held: {carried:?}"
    );
}
