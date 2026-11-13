// SPDX-License-Identifier: AGPL-3.0-or-later
//! A one-image TLS registry over HTTP/1.1 (one request per connection) that records what it saw,
//! for the tests that pull through the pull proxy.
#![expect(
    clippy::unwrap_used,
    reason = "helpers outside #[test] functions fail the test by panicking"
)]

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

use super::sha256;

pub(crate) const REPO: &str = "puddle/hello";

/// A one-file image: (manifest, blobs by digest). The layer is an uncompressed tar.
pub(crate) fn image() -> (Vec<u8>, BTreeMap<String, Vec<u8>>) {
    let mut layer = tar::Builder::new(Vec::new());
    let content = b"hello from the lab registry\n";
    let mut header = tar::Header::new_gnu();
    header.set_size(content.len() as u64);
    header.set_mode(0o755);
    header.set_cksum();
    layer
        .append_data(&mut header, "hello", &content[..])
        .unwrap();
    let layer = layer.into_inner().unwrap();
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        _ => "amd64",
    };
    let config = serde_json::to_vec(&serde_json::json!({
        "architecture": arch,
        "os": "linux",
        "config": { "Env": ["PATH=/usr/bin:/bin", "PUDDLE_LAB=1"], "Cmd": ["/hello"] },
        "rootfs": { "type": "layers", "diff_ids": [sha256(&layer)] },
    }))
    .unwrap();
    let manifest = serde_json::to_vec(&serde_json::json!({
        "schemaVersion": 2,
        "mediaType": "application/vnd.oci.image.manifest.v1+json",
        "config": {
            "mediaType": "application/vnd.oci.image.config.v1+json",
            "digest": sha256(&config),
            "size": config.len(),
        },
        "layers": [{
            "mediaType": "application/vnd.oci.image.layer.v1.tar",
            "digest": sha256(&layer),
            "size": layer.len(),
        }],
    }))
    .unwrap();
    let blobs = BTreeMap::from([(sha256(&config), config), (sha256(&layer), layer)]);
    (manifest, blobs)
}

/// What the lab registry saw.
#[derive(Default)]
pub(crate) struct Seen {
    pub(crate) handshakes_failed: AtomicUsize,
    pub(crate) requests: Mutex<Vec<String>>,
}

impl Seen {
    pub(crate) fn requests(&self) -> Vec<String> {
        self.requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// Serves the OCI distribution API for [`image`] over TLS, one request per connection.
pub(crate) async fn registry(
    listener: TcpListener,
    tls: Arc<rustls::ServerConfig>,
    seen: Arc<Seen>,
) {
    let acceptor = TlsAcceptor::from(tls);
    let (manifest, blobs) = image();
    let manifest_digest = sha256(&manifest);
    let (manifest, blobs) = (Arc::new(manifest), Arc::new(blobs));
    while let Ok((tcp, _)) = listener.accept().await {
        let (acceptor, seen) = (acceptor.clone(), Arc::clone(&seen));
        let (manifest, blobs, manifest_digest) = (
            Arc::clone(&manifest),
            Arc::clone(&blobs),
            manifest_digest.clone(),
        );
        tokio::spawn(async move {
            let Ok(tls) = acceptor.accept(tcp).await else {
                seen.handshakes_failed.fetch_add(1, Ordering::SeqCst);
                return;
            };
            let mut reader = BufReader::new(tls);
            let mut line = String::new();
            if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                return;
            }
            loop {
                let mut header = String::new();
                if reader.read_line(&mut header).await.unwrap_or(0) == 0 || header == "\r\n" {
                    break;
                }
            }
            let mut parts = line.split_whitespace();
            let (method, path) = (
                parts.next().unwrap_or_default().to_owned(),
                parts.next().unwrap_or_default().to_owned(),
            );
            seen.requests
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(format!("{method} {path}"));
            let manifests = format!("/v2/{REPO}/manifests/");
            let blob_prefix = format!("/v2/{REPO}/blobs/");
            let (status, kind, body): (&str, &str, Vec<u8>) = if path == "/v2/" {
                ("200 OK", "application/json", b"{}".to_vec())
            } else if let Some(r) = path.strip_prefix(&manifests)
                && (r == "1" || r == manifest_digest)
            {
                (
                    "200 OK",
                    "application/vnd.oci.image.manifest.v1+json",
                    manifest.to_vec(),
                )
            } else if let Some(blob) = path.strip_prefix(&blob_prefix).and_then(|d| blobs.get(d)) {
                ("200 OK", "application/octet-stream", blob.clone())
            } else {
                ("404 Not Found", "application/json", b"{}".to_vec())
            };
            let head = format!(
                "HTTP/1.1 {status}\r\ncontent-type: {kind}\r\ncontent-length: {}\r\ndocker-content-digest: {}\r\nconnection: close\r\n\r\n",
                body.len(),
                if kind.contains("manifest") {
                    manifest_digest.as_str()
                } else {
                    ""
                },
            );
            let tls = reader.get_mut();
            let _ = tls.write_all(head.as_bytes()).await;
            if method != "HEAD" {
                let _ = tls.write_all(&body).await;
            }
            let _ = tls.shutdown().await;
        });
    }
}
