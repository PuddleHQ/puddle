// SPDX-License-Identifier: GPL-3.0-or-later
//! A fake website: a server that answers `200` to any request, over plain HTTP and over TLS
//! (HTTP/2 or HTTP/1.1 by ALPN), for every name the test gives it. It stands in for "some
//! allowed host" in tests that only need a host to reach, so they never depend on a public site
//! being up. Reach it by name through a [`NameProxy`](super::NameProxy) that
//! [`WebHost::register`] fills. Every request is recorded as a [`Seen`](super::Seen), headers
//! included, so a test can say what the server was sent (a credential the proxy swapped in, say).
//!
//! The certificate's root is named so that it never contains the product name: a test that looks
//! at the issuer a guest sees can tell Puddle's certificate from the fixture's. Test code only.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex, PoisonError};

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::{TokioExecutor, TokioIo};
use rustls::pki_types::CertificateDer;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio_rustls::TlsAcceptor;

use super::git_host::{Seen, Versions, tls_config};
use super::{LOCAL, NameProxy};

/// An answer a [`Responder`] gives instead of the default `200` with the target echoed.
#[derive(Clone, Debug)]
pub struct WebAnswer {
    /// The status code.
    pub status: u16,
    /// The `content-type`.
    pub content_type: &'static str,
    /// The body.
    pub body: Vec<u8>,
}

/// Decides the answer to a request from what the site saw of it (headers, with the `host` first,
/// and the body); `None` is the default answer. A site that stands in for a service with an API
/// of its own (a token endpoint) uses it.
pub type Responder = Arc<dyn Fn(&Seen) -> Option<WebAnswer> + Send + Sync>;

/// What the fake website answers with.
#[derive(Clone, Default)]
pub struct WebOptions {
    /// An `alt-svc` header value sent on every TLS answer (a site that offers HTTP/3).
    pub alt_svc: Option<String>,
    /// Answers some requests itself (see [`Responder`]).
    pub responder: Option<Responder>,
}

impl std::fmt::Debug for WebOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebOptions")
            .field("alt_svc", &self.alt_svc)
            .field("responder", &self.responder.as_ref().map(|_| "..."))
            .finish()
    }
}

struct State {
    options: WebOptions,
    seen: Mutex<Vec<Seen>>,
}

/// The fake website. Stops when dropped.
pub struct WebHost {
    plain: SocketAddr,
    tls: SocketAddr,
    root: CertificateDer<'static>,
    state: Arc<State>,
    tasks: Vec<JoinHandle<()>>,
}

impl Drop for WebHost {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

impl WebHost {
    /// Starts a site whose certificate names every one of `names`.
    pub async fn start(names: &[&str], options: WebOptions) -> Self {
        let names: Vec<String> = names.iter().map(|n| (*n).to_owned()).collect();
        let (root, config) = tls_config("Lab web root", &names, Versions::Both, (2020, 2090));
        let state = Arc::new(State {
            options,
            seen: Mutex::new(Vec::new()),
        });
        let plain_listener = TcpListener::bind((LOCAL, 0)).await.unwrap();
        let tls_listener = TcpListener::bind((LOCAL, 0)).await.unwrap();
        let (plain, tls) = (
            plain_listener.local_addr().unwrap(),
            tls_listener.local_addr().unwrap(),
        );
        let acceptor = TlsAcceptor::from(config);
        let tasks = vec![
            tokio::spawn(serve_plain(plain_listener, state.clone())),
            tokio::spawn(serve_tls(tls_listener, acceptor, state.clone())),
        ];
        Self {
            plain,
            tls,
            root,
            state,
            tasks,
        }
    }

    /// Sends `name` on port 80 to the plain listener and on port 443 to the TLS one, for each
    /// name of `names`, in `proxy`.
    pub fn register(&self, proxy: &NameProxy, names: &[&str]) {
        for name in names {
            proxy.route(&format!("{name}:80"), self.plain);
            proxy.route(&format!("{name}:443"), self.tls);
        }
    }

    /// The root that signed the site's certificate.
    #[must_use]
    pub fn root(&self) -> &CertificateDer<'static> {
        &self.root
    }

    /// Where the plain-HTTP listener is.
    #[must_use]
    pub fn plain_addr(&self) -> SocketAddr {
        self.plain
    }

    /// Where the TLS listener is.
    #[must_use]
    pub fn tls_addr(&self) -> SocketAddr {
        self.tls
    }

    /// Every request answered so far, oldest first.
    #[must_use]
    pub fn seen(&self) -> Vec<Seen> {
        self.state
            .seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

async fn serve_plain(listener: TcpListener, state: Arc<State>) {
    loop {
        let (tcp, _) = listener.accept().await.unwrap();
        let state = state.clone();
        tokio::spawn(async move {
            let service = service_fn(move |request| {
                let state = state.clone();
                async move {
                    Ok::<_, std::convert::Infallible>(
                        handle(&state, request, "HTTP/1.1", false).await,
                    )
                }
            });
            let _ = hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(tcp), service)
                .await;
        });
    }
}

async fn serve_tls(listener: TcpListener, acceptor: TlsAcceptor, state: Arc<State>) {
    loop {
        let (tcp, _) = listener.accept().await.unwrap();
        let (acceptor, state) = (acceptor.clone(), state.clone());
        tokio::spawn(async move {
            let Ok(tls) = acceptor.accept(tcp).await else {
                return;
            };
            let h2 = tls.get_ref().1.alpn_protocol() == Some(b"h2");
            let version = if h2 { "HTTP/2" } else { "HTTP/1.1" };
            let service = service_fn(move |request| {
                let state = state.clone();
                async move {
                    Ok::<_, std::convert::Infallible>(handle(&state, request, version, true).await)
                }
            });
            let io = TokioIo::new(tls);
            if h2 {
                let _ = hyper::server::conn::http2::Builder::new(TokioExecutor::new())
                    .serve_connection(io, service)
                    .await;
            } else {
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(io, service)
                    .await;
            }
        });
    }
}

async fn handle(
    state: &State,
    request: Request<Incoming>,
    version: &'static str,
    tls: bool,
) -> Response<Full<Bytes>> {
    let (parts, body) = request.into_parts();
    let body = body
        .collect()
        .await
        .map(http_body_util::Collected::to_bytes)
        .unwrap_or_default();
    let mut headers: Vec<(String, String)> = parts
        .headers
        .iter()
        .map(|(n, v)| {
            (
                n.as_str().to_owned(),
                String::from_utf8_lossy(v.as_bytes()).into_owned(),
            )
        })
        .collect();
    if let Some(authority) = parts.uri.authority() {
        // HTTP/2's `:authority`, and an absolute URI, are the one place a Host lives.
        headers.insert(0, ("host".to_owned(), authority.to_string()));
    }
    let target = parts
        .uri
        .path_and_query()
        .map_or_else(|| parts.uri.path().to_owned(), ToString::to_string);
    let mut answer = Response::builder()
        .status(200)
        .header("content-type", "text/plain")
        .header("x-lab-web-host", "answered");
    if let (true, Some(alt_svc)) = (tls, &state.options.alt_svc) {
        answer = answer.header("alt-svc", alt_svc);
    }
    let mut seen = Seen {
        method: parts.method.as_str().to_owned(),
        target,
        headers,
        version,
        status: 200,
        body: body.iter().copied().take(64 * 1024).collect(),
    };
    let chosen = state.options.responder.as_ref().and_then(|ask| ask(&seen));
    let answer = if let Some(chosen) = chosen {
        seen.status = chosen.status;
        Response::builder()
            .status(chosen.status)
            .header("content-type", chosen.content_type)
            .header("x-lab-web-host", "answered")
            .body(Full::new(Bytes::from(chosen.body)))
            .unwrap()
    } else {
        answer
            .body(Full::new(Bytes::from(format!(
                "lab web host: {}\n",
                seen.target
            ))))
            .unwrap()
    };
    state
        .seen
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push(seen);
    answer
}
