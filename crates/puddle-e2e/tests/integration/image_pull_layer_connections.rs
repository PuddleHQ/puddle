// SPDX-License-Identifier: GPL-3.0-or-later
//! A CDN in front of a registry may hold back the answer to a request that arrives on an HTTP/2
//! connection busy with another layer's body, then cancel the stream (`RST_STREAM` with
//! `CANCEL`, before any headers) when its wait runs out. mcr.microsoft.com's data endpoint does
//! that on a slow connection after about 10 s, and every retry on the same connection queues
//! behind the same bodies. So the pull must not let two layers share a connection: here a
//! layer's body finishes only once every layer's body has started, and a layer request on a
//! connection that already carries a layer body is held and cancelled. The adapter's real
//! `pull_image` goes through Puddle's [`PullProxy`] to a TLS registry that speaks only HTTP/2.
#![expect(
    clippy::unwrap_used,
    reason = "helpers outside #[test] functions fail the test by panicking"
)]

use std::collections::{BTreeMap, BTreeSet};
use std::future::poll_fn;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use bytes::Bytes;
use h2::server::SendResponse;
use h2::{Reason, SendStream};
use puddle_compute::Runtime;
use puddle_compute_msb::{MsbConfig, MsbRuntime};
use puddle_netpolicy::PuddleEndpoints;
use puddle_proxy::PullProxy;
use puddle_types::ImageRef;
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

use crate::lab;

use lab::{REGISTRY, sha256};

const REPO: &str = "puddle/layers";

/// How long a layer request waits on a busy connection before the registry cancels it.
const HOLD: Duration = Duration::from_millis(300);

/// How long a started layer body waits for the other layers' bodies to start. Past it the body
/// finishes anyway, so a client that shares a connection gets an answer instead of a hang.
const ALL_STARTED_CAP: Duration = Duration::from_secs(20);

fn tar_layer(name: &str, len: usize) -> Vec<u8> {
    #[expect(clippy::cast_possible_truncation, reason = "a repeating byte pattern")]
    let content: Vec<u8> = (0..len).map(|i| (i.wrapping_mul(7) >> 2) as u8).collect();
    let mut layer = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_size(content.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    layer.append_data(&mut header, name, &content[..]).unwrap();
    layer.into_inner().unwrap()
}

/// A two-layer image: (manifest, layers by digest, config).
fn image() -> (Vec<u8>, BTreeMap<String, Vec<u8>>, Vec<u8>) {
    let layers = [
        tar_layer("one.bin", 64 * 1024),
        tar_layer("two.bin", 96 * 1024),
    ];
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        _ => "amd64",
    };
    let config = serde_json::to_vec(&serde_json::json!({
        "architecture": arch,
        "os": "linux",
        "config": { "Env": ["PATH=/usr/bin:/bin", "PUDDLE_LAB=layers"], "Cmd": ["/one"] },
        "rootfs": { "type": "layers", "diff_ids": layers.iter().map(|l| sha256(l)).collect::<Vec<_>>() },
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
        "layers": layers.iter().map(|l| serde_json::json!({
            "mediaType": "application/vnd.oci.image.layer.v1.tar",
            "digest": sha256(l),
            "size": l.len(),
        })).collect::<Vec<_>>(),
    }))
    .unwrap();
    let layers = layers.into_iter().map(|l| (sha256(&l), l)).collect();
    (manifest, layers, config)
}

struct Registry {
    manifest: Vec<u8>,
    layers: BTreeMap<String, Vec<u8>>,
    config: Vec<u8>,
    /// Layers whose body has started.
    started: Mutex<BTreeSet<String>>,
    /// Layer requests cancelled because their connection was busy.
    cancelled: AtomicUsize,
}

impl Registry {
    fn all_started(&self) -> bool {
        self.started
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
            == self.layers.len()
    }
}

/// Layer bodies in progress on one connection.
#[derive(Default)]
struct Connection {
    layer_bodies: AtomicUsize,
}

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

async fn wait_until(deadline: Duration, done: impl Fn() -> bool) -> bool {
    let start = Instant::now();
    while !done() {
        if start.elapsed() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    true
}

async fn serve_layer(
    registry: &Registry,
    connection: &Connection,
    digest: &str,
    blob: &[u8],
    mut respond: SendResponse<Bytes>,
) {
    let busy = || connection.layer_bodies.load(Ordering::SeqCst) > 0;
    if busy() && !wait_until(HOLD, || !busy()).await {
        registry.cancelled.fetch_add(1, Ordering::SeqCst);
        respond.send_reset(Reason::CANCEL);
        return;
    }
    connection.layer_bodies.fetch_add(1, Ordering::SeqCst);
    let headers = [
        ("content-type", "application/octet-stream".to_owned()),
        ("content-length", blob.len().to_string()),
    ];
    let mut out = respond
        .send_response(response(200, &headers), false)
        .unwrap();
    let (head, last) = blob.split_at(blob.len() - 1);
    send_all(&mut out, Bytes::copy_from_slice(head)).await;
    registry
        .started
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(digest.to_owned());
    wait_until(ALL_STARTED_CAP, || registry.all_started()).await;
    out.send_data(Bytes::copy_from_slice(last), true).unwrap();
    connection.layer_bodies.fetch_sub(1, Ordering::SeqCst);
}

/// Answers one request of the OCI distribution API.
async fn answer(
    registry: Arc<Registry>,
    connection: Arc<Connection>,
    request: http::Request<h2::RecvStream>,
    mut respond: SendResponse<Bytes>,
) {
    let path = request.uri().path().to_owned();
    let head_only = request.method() == http::Method::HEAD;
    let blob_digest = path.strip_prefix(&format!("/v2/{REPO}/blobs/"));
    if path == "/v2/" {
        let mut out = respond
            .send_response(response(200, &[]), head_only)
            .unwrap();
        if !head_only {
            out.send_data(Bytes::from_static(b"{}"), true).unwrap();
        }
    } else if path.starts_with(&format!("/v2/{REPO}/manifests/")) {
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
    } else if let Some((digest, blob)) = blob_digest.and_then(|d| registry.layers.get_key_value(d))
    {
        serve_layer(&registry, &connection, digest, blob, respond).await;
    } else if blob_digest == Some(sha256(&registry.config).as_str()) {
        let headers = [("content-length", registry.config.len().to_string())];
        let mut out = respond
            .send_response(response(200, &headers), false)
            .unwrap();
        out.send_data(Bytes::from(registry.config.clone()), true)
            .unwrap();
    } else {
        let _ = respond.send_response(response(404, &[]), true);
    }
}

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
            let connection = Arc::new(Connection::default());
            while let Some(Ok((request, respond))) = conn.accept().await {
                let (registry, connection) = (Arc::clone(&registry), Arc::clone(&connection));
                tokio::spawn(answer(registry, connection, request, respond));
            }
        });
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn layers_that_would_queue_behind_each_other_on_one_connection_still_pull() {
    let (manifest, layers, config) = image();
    let registry = Arc::new(Registry {
        manifest,
        layers,
        config,
        started: Mutex::default(),
        cancelled: AtomicUsize::new(0),
    });
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
    let _route = proxy.serve().unwrap();
    let home = tempfile::tempdir().unwrap();
    // Pulling needs no msb binary: the paths only go into msb's config.json.
    let msb = MsbConfig::new(
        home.path().join("msb-home"),
        home.path().join("runtime/msb"),
        home.path().join("runtime/libkrunfw"),
        home.path().join("guest-share"),
    )
    .with_registry_proxy(proxy_url)
    .with_registry_roots([root]);
    let runtime = MsbRuntime::open(msb).await.unwrap();
    let image = ImageRef::new(&format!("{REGISTRY}/{REPO}:1")).unwrap();

    let pulled = runtime.pull_image(&image).await.unwrap_or_else(|err| {
        let cancelled = registry.cancelled.load(Ordering::SeqCst);
        panic!("pull failed ({cancelled} layer requests cancelled on a busy connection): {err}")
    });
    assert_eq!(pulled.env_var("PUDDLE_LAB"), Some("layers"));
}
