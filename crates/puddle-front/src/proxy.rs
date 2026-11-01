// SPDX-License-Identifier: GPL-3.0-or-later
//! One request through the front: route by `Host`, refuse what does not belong, then carry it to
//! the workspace's editor server and the answer back, WebSocket upgrades included.

use std::convert::Infallible;
use std::sync::Arc;

use bytes::Bytes;
use http::header::{
    self, CONNECTION, CONTENT_SECURITY_POLICY, CONTENT_TYPE, HOST, ORIGIN, UPGRADE,
};
use http::{HeaderMap, HeaderName, HeaderValue, Method, Request, Response, StatusCode, Uri};
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper_util::rt::TokioIo;
use puddle_types::WorkspaceName;
use tokio::sync::watch;

use crate::editor::{Flavour, Guests, Target};
use crate::host_header::{Refusal, Route, origin_of, route_of};
use crate::keys::ServerHalves;
use crate::microsoft;
use crate::windows::Windows;

/// What a request handler needs, shared by every connection.
pub(crate) struct Shared {
    pub(crate) guests: Arc<dyn Guests>,
    pub(crate) halves: Arc<dyn ServerHalves>,
    pub(crate) windows: Windows,
    pub(crate) port: u16,
    pub(crate) stop: watch::Receiver<bool>,
    /// One permit per upgraded socket: hyper stops counting a connection once it is upgraded.
    pub(crate) sockets: Arc<tokio::sync::Semaphore>,
}

/// How long a server may take to answer with its first response head.
const ANSWER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

type Body = BoxBody<Bytes, hyper::Error>;

/// Added to every response so the editor cannot be framed by another origin.
const FRAME_ANCESTORS: &str = "frame-ancestors 'self'";

/// Headers that belong to one connection and are never passed on.
const HOP_BY_HOP: [HeaderName; 6] = [
    HeaderName::from_static("keep-alive"),
    header::PROXY_AUTHENTICATE,
    header::PROXY_AUTHORIZATION,
    header::TE,
    header::TRAILER,
    header::TRANSFER_ENCODING,
];

pub(crate) async fn handle(
    shared: Arc<Shared>,
    request: Request<Incoming>,
) -> Result<Response<Body>, Infallible> {
    Ok(respond(&shared, request).await)
}

fn text(status: StatusCode, message: impl Into<String>) -> Response<Body> {
    let mut response = Response::new(
        Full::new(Bytes::from(message.into()))
            .map_err(never)
            .boxed(),
    );
    *response.status_mut() = status;
    let headers = response.headers_mut();
    headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    // Some of these texts carry what the guest said; a page of its own could run script otherwise.
    headers.insert(
        CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'; sandbox"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

fn never(never: Infallible) -> hyper::Error {
    match never {}
}

async fn respond(shared: &Shared, request: Request<Incoming>) -> Response<Body> {
    if request.headers().get_all(HOST).iter().count() > 1 {
        return text(StatusCode::BAD_REQUEST, "More than one Host header.");
    }
    let host = request
        .headers()
        .get(HOST)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let Route {
        workspace,
        guest_port,
    } = match route_of(host.as_deref(), shared.port) {
        Ok(route) => route,
        Err(Refusal::Missing) => {
            return text(StatusCode::BAD_REQUEST, "The request names no host.");
        }
        Err(_) => {
            return text(
                StatusCode::MISDIRECTED_REQUEST,
                "This is not the address of a Puddle editor.",
            );
        }
    };
    let own_origin = origin_of(host.as_deref().unwrap_or_default());
    let asked = Asked {
        host: host.as_deref().unwrap_or_default(),
        own_origin: &own_origin,
        workspace: &workspace,
        forwarded: guest_port.is_some(),
        front_port: shared.port,
    };
    if let Some(wrong) = misdirected(&request, &asked) {
        return wrong;
    }
    let Some(target) = shared.guests.target(&workspace) else {
        return text(
            StatusCode::NOT_FOUND,
            format!("No running workspace is named {workspace}."),
        );
    };
    if request.method() == Method::CONNECT {
        return text(StatusCode::METHOD_NOT_ALLOWED, "CONNECT is not served.");
    }
    if guest_port.is_none()
        && target.flavour == Flavour::Microsoft
        && request.uri().path() == microsoft::MINT_PATH
    {
        return mint(shared, &workspace, request.method(), request.headers()).await;
    }
    forward(shared, &workspace, &target, guest_port, request).await
}

/// Who a request was addressed to, for [`misdirected`].
struct Asked<'a> {
    host: &'a str,
    own_origin: &'a str,
    workspace: &'a WorkspaceName,
    /// The `Host` is a forwarded port's, not the editor's.
    forwarded: bool,
    front_port: u16,
}

impl Asked<'_> {
    /// Whether a request from the page at `origin` may use this address: its own origin, and, on a
    /// forwarded port, the origin of another forwarded port of the same workspace (an app's page
    /// calls its API on a port beside it). Never the editor's origin, which holds the sign-in.
    fn allows(&self, origin: &str) -> bool {
        if origin == self.own_origin {
            return true;
        }
        let sibling = origin
            .strip_prefix("http://")
            .and_then(|host| route_of(Some(host), self.front_port).ok());
        self.forwarded
            && sibling.is_some_and(|r| r.guest_port.is_some() && &r.workspace == self.workspace)
    }
}

/// A refusal for a request that names another origin than the page's own, or another authority in
/// its target than in its `Host`.
fn misdirected(request: &Request<Incoming>, asked: &Asked<'_>) -> Option<Response<Body>> {
    let target_differs = request
        .uri()
        .authority()
        .is_some_and(|a| !a.as_str().eq_ignore_ascii_case(asked.host));
    if target_differs {
        return Some(text(
            StatusCode::MISDIRECTED_REQUEST,
            "The request target and the Host differ.",
        ));
    }
    let origin = request.headers().get(ORIGIN)?;
    (!origin.to_str().is_ok_and(|origin| asked.allows(origin))).then(|| {
        text(
            StatusCode::FORBIDDEN,
            "Another origin may not use this editor.",
        )
    })
}

async fn mint(
    shared: &Shared,
    workspace: &WorkspaceName,
    method: &Method,
    headers: &HeaderMap,
) -> Response<Body> {
    if method != Method::POST {
        let mut response = text(StatusCode::METHOD_NOT_ALLOWED, "The key is minted by POST.");
        response
            .headers_mut()
            .insert(header::ALLOW, HeaderValue::from_static("POST"));
        return response;
    }
    let halves = Arc::clone(&shared.halves);
    let name = workspace.clone();
    let server = tokio::task::spawn_blocking(move || halves.half(&name)).await;
    let server = match server {
        Ok(Ok(half)) => half,
        Ok(Err(err)) => return key_trouble(workspace, &err.to_string()),
        Err(err) => return key_trouble(workspace, &err.to_string()),
    };
    let client = match microsoft::client_half(headers) {
        Some(half) => half,
        None => match microsoft::new_client_half() {
            Ok(half) => half,
            Err(err) => return key_trouble(workspace, &err.to_string()),
        },
    };
    let key = microsoft::key(&server, &client);
    let mut response = Response::new(
        Full::new(Bytes::copy_from_slice(&key))
            .map_err(never)
            .boxed(),
    );
    let out = response.headers_mut();
    out.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    out.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    add_frame_headers(out);
    for cookie in microsoft::set_cookies(&client) {
        out.append(header::SET_COOKIE, cookie);
    }
    response
}

fn key_trouble(workspace: &WorkspaceName, why: &str) -> Response<Body> {
    tracing::warn!(%workspace, error = %why, "the editor's secret key could not be made");
    text(
        StatusCode::INTERNAL_SERVER_ERROR,
        "The key for the editor's stored secrets could not be made.",
    )
}

fn add_frame_headers(headers: &mut HeaderMap) {
    headers.append(
        CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(FRAME_ANCESTORS),
    );
    headers.insert(
        header::X_FRAME_OPTIONS,
        HeaderValue::from_static("SAMEORIGIN"),
    );
}

fn is_upgrade(headers: &HeaderMap) -> bool {
    let wants = |name: &HeaderName, word: &str| {
        headers
            .get_all(name)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .flat_map(|v| v.split(','))
            .any(|part| part.trim().eq_ignore_ascii_case(word))
    };
    wants(&CONNECTION, "upgrade") && wants(&UPGRADE, "websocket")
}

fn bad_gateway(workspace: &WorkspaceName, guest_port: Option<u16>, why: &str) -> Response<Body> {
    let what = guest_port.map_or_else(|| "The editor".to_owned(), |p| format!("Port {p}"));
    text(
        StatusCode::BAD_GATEWAY,
        format!("{what} of workspace {workspace} is not reachable: {why}"),
    )
}

async fn forward(
    shared: &Shared,
    workspace: &WorkspaceName,
    target: &Target,
    guest_port: Option<u16>,
    mut request: Request<Incoming>,
) -> Response<Body> {
    let upgrade = is_upgrade(request.headers());
    let socket = if upgrade {
        match Arc::clone(&shared.sockets).try_acquire_owned() {
            Ok(permit) => Some(permit),
            Err(_) => return bad_gateway(workspace, guest_port, "too many sockets are open"),
        }
    } else {
        None
    };
    let connected = match guest_port {
        Some(port) => target.editor.connect_port(port).await,
        None => target.editor.connect().await,
    };
    let io = match connected {
        Ok(io) => io,
        Err(why) => return bad_gateway(workspace, guest_port, &why.0),
    };
    let (mut sender, connection) =
        match hyper::client::conn::http1::handshake(TokioIo::new(io)).await {
            Ok(pair) => pair,
            Err(err) => return bad_gateway(workspace, guest_port, &err.to_string()),
        };
    tokio::spawn(async move {
        if let Err(err) = connection.with_upgrades().await {
            tracing::debug!(error = %err, "an editor connection ended with an error");
        }
    });

    // A forwarded port is the guest's own app: no secret-storage cookie, no frame headers (its
    // own policy stands), and its sockets are not editor windows that hold up a server update.
    let editor = guest_port.is_none();
    let microsoft = editor && target.flavour == Flavour::Microsoft;
    let client_half = if microsoft {
        microsoft::client_half(request.headers())
    } else {
        None
    };
    let client_upgrade = upgrade.then(|| hyper::upgrade::on(&mut request));
    prepare_request(&mut request, upgrade, microsoft);
    let mut response =
        match tokio::time::timeout(ANSWER_TIMEOUT, sender.send_request(request)).await {
            Ok(Ok(response)) => response,
            Ok(Err(err)) => {
                return bad_gateway(workspace, guest_port, &format!("it did not answer ({err})"));
            }
            Err(_) => return bad_gateway(workspace, guest_port, "it did not answer in time"),
        };

    if upgrade && response.status() == StatusCode::SWITCHING_PROTOCOLS {
        if let Some(client_upgrade) = client_upgrade {
            let server_upgrade = hyper::upgrade::on(&mut response);
            let open = editor.then(|| shared.windows.open(workspace));
            let mut stop = shared.stop.clone();
            tokio::spawn(async move {
                let _socket = socket;
                let (Ok(client), Ok(server)) = tokio::join!(client_upgrade, server_upgrade) else {
                    return;
                };
                tokio::select! {
                    _ = stop.wait_for(|stopped| *stopped) => {}
                    () = carry(TokioIo::new(client), TokioIo::new(server), open) => {}
                }
            });
        }
        return response.map(BodyExt::boxed);
    }

    let headers = response.headers_mut();
    remove_hop_by_hop(headers, false);
    if editor {
        add_frame_headers(headers);
    }
    if microsoft {
        let client = match client_half {
            Some(half) => half,
            None => match microsoft::new_client_half() {
                Ok(half) => half,
                Err(err) => return key_trouble(workspace, &err.to_string()),
            },
        };
        for cookie in microsoft::set_cookies(&client) {
            headers.append(header::SET_COOKIE, cookie);
        }
    }
    response.map(BodyExt::boxed)
}

/// How long one side of an upgraded socket is carried on after the other side ended it.
const LINGER: std::time::Duration = std::time::Duration::from_secs(5);

/// Carries an upgraded socket both ways. The window is gone once the client ends its side, so
/// `open` is dropped then, whatever the server does with its own; either side's end leaves the
/// other [`LINGER`] to finish before the socket closes.
async fn carry<C, S>(client: C, server: S, open: Option<crate::windows::Connected>)
where
    C: tokio::io::AsyncRead + tokio::io::AsyncWrite,
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite,
{
    use tokio::io::AsyncWriteExt as _;
    let (mut client_in, mut client_out) = tokio::io::split(client);
    let (mut server_in, mut server_out) = tokio::io::split(server);
    let up = async {
        let copied = tokio::io::copy(&mut client_in, &mut server_out).await;
        let _ = server_out.shutdown().await;
        copied.inspect_err(socket_error)
    };
    let down = async {
        let copied = tokio::io::copy(&mut server_in, &mut client_out).await;
        let _ = client_out.shutdown().await;
        copied.inspect_err(socket_error)
    };
    tokio::pin!(up, down);
    // A linger that runs out ends the socket like a close.
    tokio::select! {
        _ = &mut up => {
            drop(open);
            let _ = tokio::time::timeout(LINGER, &mut down).await;
        }
        _ = &mut down => {
            let _ = tokio::time::timeout(LINGER, &mut up).await;
        }
    }
}

/// Logs an error that ended one side of an editor socket (a reset ends it like a close).
fn socket_error(err: &std::io::Error) {
    tracing::debug!(error = %err, "an editor socket ended with an error");
}

/// Removes the headers of the client's connection; an upgrade keeps the two that make it one.
fn remove_hop_by_hop(headers: &mut HeaderMap, upgrade: bool) {
    for name in &HOP_BY_HOP {
        headers.remove(name);
    }
    if !upgrade {
        headers.remove(CONNECTION);
        headers.remove(UPGRADE);
    }
}

/// Makes the client's request the one the server gets: origin-form target, no hop-by-hop headers,
/// and without the client half of Microsoft's key.
fn prepare_request(request: &mut Request<Incoming>, upgrade: bool, microsoft: bool) {
    remove_hop_by_hop(request.headers_mut(), upgrade);
    if microsoft {
        microsoft::strip_client_half(request.headers_mut());
    }
    let target = request
        .uri()
        .path_and_query()
        .map_or("/", http::uri::PathAndQuery::as_str);
    if let Ok(uri) = target.parse::<Uri>() {
        *request.uri_mut() = uri;
    }
}
