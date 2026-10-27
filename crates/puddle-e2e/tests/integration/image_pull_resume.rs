// SPDX-License-Identifier: GPL-3.0-or-later
//! A registry that cancels an HTTP/2 stream in the middle of a layer download (`RST_STREAM`
//! with `CANCEL`, as a CDN does now and then) must not fail the pull: the adapter's real
//! `pull_image` goes through Puddle's [`PullProxy`] to a TLS registry that speaks only HTTP/2,
//! the first download of the layer is cut after part of its bytes, and the pull comes back with
//! the image's config after the layer's download resumed with a `Range` request instead of
//! restarting. A registry that never stops cancelling ends in an error that says why.
#![expect(
    clippy::unwrap_used,
    reason = "helpers outside #[test] functions fail the test by panicking"
)]

use std::collections::BTreeMap;
use std::future::poll_fn;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use bytes::Bytes;
use h2::server::SendResponse;
use h2::{Reason, SendStream};
use puddle_compute::{ComputeError, Runtime};
use puddle_compute_msb::{MsbConfig, MsbRuntime};
use puddle_netpolicy::PuddleEndpoints;
use puddle_proxy::PullProxy;
use puddle_types::ImageRef;
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

use crate::lab;

use lab::{REGISTRY, sha256};

const REPO: &str = "puddle/big";

/// The layer: one file of this many bytes, so the cut falls well inside it.
const LAYER_FILE_BYTES: usize = 600 * 1024;

/// How many bytes of the layer the registry sends before it cancels the first download.
const CUT_AFTER: usize = 200 * 1024;

/// A one-file image with a large uncompressed layer: (manifest, blobs by digest).
fn image() -> (Vec<u8>, BTreeMap<String, Vec<u8>>) {
    // Bytes the compressor-free layer can't make a mistake about: a counter pattern.
    #[expect(clippy::cast_possible_truncation, reason = "a repeating byte pattern")]
    let content: Vec<u8> = (0..LAYER_FILE_BYTES)
        .map(|i| (i.wrapping_mul(31) >> 3) as u8)
        .collect();
    let mut layer = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_size(content.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    layer
        .append_data(&mut header, "big.bin", &content[..])
        .unwrap();
    let layer = layer.into_inner().unwrap();
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        _ => "amd64",
    };
    let config = serde_json::to_vec(&serde_json::json!({
        "architecture": arch,
        "os": "linux",
        "config": { "Env": ["PATH=/usr/bin:/bin", "PUDDLE_LAB=resume"], "Cmd": ["/big"] },
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

/// What the registry saw and how it behaves.
struct Registry {
    manifest: Vec<u8>,
    blobs: BTreeMap<String, Vec<u8>>,
    layer_digest: String,
    /// How many layer downloads to cut before one is served whole (`usize::MAX`: all of them).
    cuts: usize,
    /// How many bytes of a cut download are sent before the stream is cancelled.
    cut_after: usize,
    /// Layer requests so far: the `Range` header each carried.
    layer_requests: Mutex<Vec<Option<String>>>,
    cut_count: AtomicUsize,
}

impl Registry {
    fn new(cuts: usize, cut_after: usize) -> Arc<Self> {
        let (manifest, blobs) = image();
        let layer_digest = blobs
            .iter()
            .max_by_key(|(_, blob)| blob.len())
            .map(|(digest, _)| digest.clone())
            .unwrap();
        Arc::new(Self {
            manifest,
            blobs,
            layer_digest,
            cuts,
            cut_after,
            layer_requests: Mutex::default(),
            cut_count: AtomicUsize::new(0),
        })
    }

    fn layer_requests(&self) -> Vec<Option<String>> {
        self.layer_requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// Sends `data` on `stream` as flow control allows.
async fn send_all(stream: &mut SendStream<Bytes>, mut data: Bytes) {
    while !data.is_empty() {
        stream.reserve_capacity(data.len());
        let granted = poll_fn(|cx| stream.poll_capacity(cx))
            .await
            .unwrap()
            .unwrap();
        let chunk = data.split_to(granted.min(data.len()));
        stream.send_data(chunk, false).unwrap();
    }
}

fn response(status: u16, headers: &[(&str, String)]) -> http::Response<()> {
    let mut builder = http::Response::builder().status(status);
    for (name, value) in headers {
        builder = builder.header(*name, value);
    }
    builder.body(()).unwrap()
}

/// Answers one request of the OCI distribution API.
async fn answer(
    registry: Arc<Registry>,
    request: http::Request<h2::RecvStream>,
    mut respond: SendResponse<Bytes>,
) {
    let path = request.uri().path().to_owned();
    let range = request
        .headers()
        .get("range")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let manifests = format!("/v2/{REPO}/manifests/");
    let blob_prefix = format!("/v2/{REPO}/blobs/");
    let head_only = request.method() == http::Method::HEAD;
    if path == "/v2/" {
        let mut out = respond
            .send_response(response(200, &[]), head_only)
            .unwrap();
        if !head_only {
            out.send_data(Bytes::from_static(b"{}"), true).unwrap();
        }
    } else if path.starts_with(&manifests) {
        let headers = [
            (
                "content-type",
                "application/vnd.oci.image.manifest.v1+json".to_owned(),
            ),
            ("docker-content-digest", sha256(&registry.manifest)),
            ("content-length", registry.manifest.len().to_string()),
        ];
        let mut out = respond
            .send_response(response(200, &headers), head_only)
            .unwrap();
        if !head_only {
            out.send_data(Bytes::from(registry.manifest.clone()), true)
                .unwrap();
        }
    } else if let Some(blob) = path
        .strip_prefix(&blob_prefix)
        .and_then(|digest| registry.blobs.get(digest))
    {
        let is_layer = path.ends_with(&registry.layer_digest);
        if is_layer {
            registry
                .layer_requests
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(range.clone());
        }
        let start = range
            .as_deref()
            .and_then(|r| r.strip_prefix("bytes="))
            .and_then(|r| r.split('-').next())
            .and_then(|n| n.parse::<usize>().ok());
        let body = Bytes::from(blob.get(start.unwrap_or(0)..).unwrap_or_default().to_vec());
        let mut headers = vec![
            ("content-type", "application/octet-stream".to_owned()),
            ("content-length", body.len().to_string()),
        ];
        let status = if let Some(start) = start {
            headers.push((
                "content-range",
                format!("bytes {start}-{}/{}", blob.len() - 1, blob.len()),
            ));
            206
        } else {
            200
        };
        let mut out = respond
            .send_response(response(status, &headers), false)
            .unwrap();
        let cut = is_layer && registry.cut_count.load(Ordering::SeqCst) < registry.cuts;
        if cut {
            registry.cut_count.fetch_add(1, Ordering::SeqCst);
            send_all(&mut out, body.slice(..registry.cut_after.min(body.len()))).await;
            // The client reads what was sent before the stream goes away.
            tokio::time::sleep(Duration::from_millis(300)).await;
            out.send_reset(Reason::CANCEL);
        } else {
            send_all(&mut out, body).await;
            out.send_data(Bytes::new(), true).unwrap();
        }
    } else {
        let mut out = respond.send_response(response(404, &[]), true).unwrap();
        let _ = &mut out;
    }
}

/// Serves HTTP/2 over TLS (the only protocol it offers) on `listener`.
async fn serve(listener: TcpListener, tls: rustls::ServerConfig, registry: Arc<Registry>) {
    let mut tls = tls;
    tls.alpn_protocols = vec![b"h2".to_vec()];
    let acceptor = TlsAcceptor::from(Arc::new(tls));
    while let Ok((tcp, _)) = listener.accept().await {
        let (acceptor, registry) = (acceptor.clone(), Arc::clone(&registry));
        tokio::spawn(async move {
            let Ok(tls) = acceptor.accept(tcp).await else {
                return;
            };
            let Ok(mut conn) = h2::server::handshake(tls).await else {
                return;
            };
            while let Some(Ok((request, respond))) = conn.accept().await {
                tokio::spawn(answer(Arc::clone(&registry), request, respond));
            }
        });
    }
}

struct Lab {
    runtime: MsbRuntime,
    image: ImageRef,
    registry: Arc<Registry>,
    _home: tempfile::TempDir,
    _route: puddle_proxy::PullRoute,
}

async fn lab(cuts: usize, cut_after: usize) -> Lab {
    let registry = Registry::new(cuts, cut_after);
    let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let addr: SocketAddr = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let (root, tls) = lab::lab_pki();
    tokio::spawn(serve(
        TcpListener::from_std(listener).unwrap(),
        tls,
        Arc::clone(&registry),
    ));
    let proxy = PullProxy::bind(&PuddleEndpoints::new())
        .unwrap()
        .with_resolver(Arc::new(lab::LabResolver(addr)));
    let proxy_url = proxy.proxy_url().expose().to_owned();
    let route = proxy.serve().unwrap();
    let home = tempfile::tempdir().unwrap();
    // Pulling needs no msb binary: the paths only go into msb's config.json.
    let config = MsbConfig::new(
        home.path().join("msb-home"),
        home.path().join("runtime/msb"),
        home.path().join("runtime/libkrunfw"),
        home.path().join("guest-share"),
    )
    .with_registry_proxy(proxy_url)
    .with_registry_roots([root]);
    Lab {
        runtime: MsbRuntime::open(config).await.unwrap(),
        image: ImageRef::new(&format!("{REGISTRY}/{REPO}:1")).unwrap(),
        registry,
        _home: home,
        _route: route,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_layer_download_cancelled_midway_resumes_and_the_pull_succeeds() {
    let lab = lab(1, CUT_AFTER).await;
    let config = lab.runtime.pull_image(&lab.image).await.unwrap();
    assert_eq!(config.env_var("PUDDLE_LAB"), Some("resume"));
    let requests = lab.registry.layer_requests();
    assert_eq!(requests.len(), 2, "{requests:?}");
    assert_eq!(requests[0], None);
    let resumed = requests[1].as_deref().unwrap();
    let offset: usize = resumed
        .strip_prefix("bytes=")
        .and_then(|r| r.strip_suffix('-'))
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("not a resume from an offset: {resumed}"));
    assert!(
        offset > 0 && offset <= CUT_AFTER,
        "resumed at {offset}, the cut was at {CUT_AFTER}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_registry_that_keeps_cancelling_fails_the_pull_with_the_reason() {
    let lab = lab(usize::MAX, 0).await;
    let err = lab.runtime.pull_image(&lab.image).await.unwrap_err();
    let ComputeError::ImagePull { reason, .. } = &err else {
        panic!("expected an image-pull error, got {err:?}");
    };
    assert!(reason.contains("stream no longer needed"), "{reason}");
    assert!(
        reason.contains("gave up after 5 attempts"),
        "the error should say it retried: {reason}"
    );
    assert!(lab.registry.layer_requests().len() >= 2);
}
