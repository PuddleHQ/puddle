// SPDX-License-Identifier: GPL-3.0-or-later
//! A fake Git host: the real `git http-backend` behind a TLS server whose root the test hands to
//! whoever must trust it. It records every request (the `Authorization` it saw, the HTTP version
//! it was spoken in) so a test can say what a client sent and what the proxy added.
//!
//! Policy, like a hosted service: a repository marked private answers `401` to a request with no
//! accepted token, and a push needs an accepted token whatever the repository. A token is
//! accepted as `Bearer <token>` or as the password of a `Basic` header (any user name), so the
//! shapes Puddle injects (`Basic x-access-token:<token>`, `Basic :<token>`) both pass.
//!
//! It also answers the small part of the Git LFS protocol a transfer needs: the batch endpoint
//! (`download` and `upload` with an `href` back on this host) and the object endpoint
//! (`GET` and `PUT`). Test code only.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, PoisonError};

use base64::Engine as _;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::{TokioExecutor, TokioIo};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::process::Command;
use tokio::task::JoinHandle;
use tokio_rustls::TlsAcceptor;

use super::LOCAL;

/// One request the host answered.
#[derive(Clone, Debug)]
pub struct Seen {
    /// `GET`, `POST`, ...
    pub method: String,
    /// The path and query as sent.
    pub target: String,
    /// The header fields, names lower-cased, in the order sent.
    pub headers: Vec<(String, String)>,
    /// `HTTP/1.1` or `HTTP/2`.
    pub version: &'static str,
    /// The status it answered.
    pub status: u16,
    /// The request body (the first 64 KiB).
    pub body: Vec<u8>,
}

impl Seen {
    /// The values of the header fields named `name` (any case).
    #[must_use]
    pub fn headers_named(&self, name: &str) -> Vec<&str> {
        self.headers
            .iter()
            .filter(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
            .collect()
    }
}

/// Which HTTP versions the host offers through ALPN.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Versions {
    /// `http/1.1` only.
    Http1,
    /// `h2` and `http/1.1`: a client that offers `h2` gets it.
    Both,
}

struct State {
    name: String,
    repos: PathBuf,
    seen: Mutex<Vec<Seen>>,
    tokens: Mutex<HashSet<String>>,
    private: Mutex<HashSet<String>>,
    objects: Mutex<HashMap<String, Vec<u8>>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// How a [`GitHost`] presents itself.
#[derive(Clone, Debug)]
pub struct GitHostOptions {
    name: String,
    certificate_names: Vec<String>,
    versions: Versions,
    valid: (i32, i32),
}

impl GitHostOptions {
    /// A host answering to `name` with a certificate for it, valid from 2020 to 2090.
    #[must_use]
    pub fn new(name: &str, versions: Versions) -> Self {
        Self {
            name: name.to_owned(),
            certificate_names: vec![name.to_owned()],
            versions,
            valid: (2020, 2090),
        }
    }

    /// The certificate names `cert_name` instead of the host's name (a host with a wrong-name
    /// certificate).
    #[must_use]
    pub fn certificate_for(mut self, cert_name: &str) -> Self {
        self.certificate_names = vec![cert_name.to_owned()];
        self
    }

    /// The certificate also names `name` (another name the same server answers to).
    #[must_use]
    pub fn also_for(mut self, name: &str) -> Self {
        self.certificate_names.push(name.to_owned());
        self
    }

    /// The certificate is valid from the start of year `from` to the start of year `to`.
    #[must_use]
    pub fn valid_between(mut self, from: i32, to: i32) -> Self {
        self.valid = (from, to);
        self
    }
}

/// The fake host. Stops when dropped.
pub struct GitHost {
    addr: SocketAddr,
    root: CertificateDer<'static>,
    state: Arc<State>,
    dir: tempfile::TempDir,
    task: JoinHandle<()>,
}

impl Drop for GitHost {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Whether `git` is on the path; a missing one skips the test unless `PUDDLE_REQUIRE_TOOLS` is
/// set (CI sets it).
///
/// # Panics
/// When `git` is missing and `PUDDLE_REQUIRE_TOOLS` is set.
#[must_use]
pub fn git_available() -> bool {
    let found = std::process::Command::new("git")
        .arg("--version")
        .output()
        .is_ok();
    assert!(
        found || std::env::var_os("PUDDLE_REQUIRE_TOOLS").is_none(),
        "PUDDLE_REQUIRE_TOOLS is set but git is not on the path"
    );
    if !found {
        // skip-ok: the assert above already failed when PUDDLE_REQUIRE_TOOLS is set
        eprintln!("skipped: git not found");
    }
    found
}

/// A `git` command with the repository variables of a running hook removed and no user or system
/// configuration, so it behaves the same on every machine.
#[must_use]
pub fn clean_git(home: &Path) -> std::process::Command {
    let mut command = std::process::Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    command
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", home.join("gitconfig"))
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null());
    command
}

fn run(mut command: std::process::Command) {
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{command:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

impl GitHost {
    /// Starts the host answering to `name`, over a root made for the run.
    pub async fn start(name: &str, versions: Versions) -> Self {
        Self::start_with(&GitHostOptions::new(name, versions)).await
    }

    /// Starts the host answering to `name` with a certificate that is valid for `cert_name`
    /// instead (a host whose certificate names another host).
    pub async fn start_named(name: &str, cert_name: &str, versions: Versions) -> Self {
        Self::start_with(&GitHostOptions::new(name, versions).certificate_for(cert_name)).await
    }

    /// Starts the host the options describe.
    pub async fn start_with(options: &GitHostOptions) -> Self {
        let name = options.name.as_str();
        let dir = tempfile::Builder::new()
            .prefix(super::Prefix::new("githost").as_str())
            .tempdir()
            .unwrap();
        let repos = dir.path().join("repos");
        std::fs::create_dir_all(&repos).unwrap();
        let (root, config) =
            tls_config(&options.certificate_names, options.versions, options.valid);
        let state = Arc::new(State {
            name: name.to_owned(),
            repos,
            seen: Mutex::new(Vec::new()),
            tokens: Mutex::new(HashSet::new()),
            private: Mutex::new(HashSet::new()),
            objects: Mutex::new(HashMap::new()),
        });
        let listener = TcpListener::bind((LOCAL, 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(accept_loop(
            listener,
            TlsAcceptor::from(config),
            state.clone(),
        ));
        Self {
            addr,
            root,
            state,
            dir,
            task,
        }
    }

    /// Where the host listens.
    #[must_use]
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// The name the host answers to.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.state.name
    }

    /// The root that signed the host's certificate.
    #[must_use]
    pub fn root(&self) -> &CertificateDer<'static> {
        &self.root
    }

    /// The root as PEM.
    #[must_use]
    pub fn root_pem(&self) -> String {
        let b64 = base64::engine::general_purpose::STANDARD.encode(self.root.as_ref());
        let mut pem = String::from("-----BEGIN CERTIFICATE-----\n");
        for line in b64.as_bytes().chunks(64) {
            pem.push_str(std::str::from_utf8(line).unwrap());
            pem.push('\n');
        }
        pem.push_str("-----END CERTIFICATE-----\n");
        pem
    }

    /// A folder for the caller's own files, removed with the host.
    #[must_use]
    pub fn scratch(&self) -> &Path {
        self.dir.path()
    }

    /// Accepts `token` from now on.
    pub fn accept_token(&self, token: &str) {
        lock(&self.state.tokens).insert(token.to_owned());
    }

    /// Stops accepting `token` (a revoked one).
    pub fn revoke_token(&self, token: &str) {
        lock(&self.state.tokens).remove(token);
    }

    /// Makes a bare repository `owner/name` (`name` without `.git`) on branch `main` with one
    /// commit holding `README`, and `private` or not.
    pub fn add_repo(&self, repo: &str, private: bool) -> PathBuf {
        let path = self.repo_path(repo);
        std::fs::create_dir_all(&path).unwrap();
        let home = self.scratch().join("home");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join("gitconfig"), "").unwrap();
        let git = |args: &[&str], cwd: &Path| {
            let mut command = clean_git(&home);
            command.current_dir(cwd).args(args);
            run(command);
        };
        git(&["init", "-q", "--bare", "-b", "main"], &path);
        git(&["config", "http.receivepack", "true"], &path);
        let work = self
            .scratch()
            .join(format!("seed-{}", repo.replace('/', "-")));
        std::fs::create_dir_all(&work).unwrap();
        git(&["init", "-q", "-b", "main"], &work);
        std::fs::write(work.join("README"), format!("{repo}\n")).unwrap();
        git(&["add", "README"], &work);
        git(
            &[
                "-c",
                "user.name=seed",
                "-c",
                "user.email=seed@example.org",
                "commit",
                "-q",
                "-m",
                "seed",
            ],
            &work,
        );
        git(&["push", "-q", path.to_str().unwrap(), "main"], &work);
        if private {
            lock(&self.state.private).insert(repo.to_owned());
        }
        path
    }

    /// The bare repository `owner/name`.
    #[must_use]
    pub fn repo_path(&self, repo: &str) -> PathBuf {
        self.state.repos.join(format!("{repo}.git"))
    }

    /// Every request answered so far.
    #[must_use]
    pub fn seen(&self) -> Vec<Seen> {
        lock(&self.state.seen).clone()
    }

    /// The `Authorization` headers of every request answered so far (one entry per request, the
    /// first header or `None`).
    #[must_use]
    pub fn authorizations(&self) -> Vec<Option<String>> {
        self.seen()
            .iter()
            .map(|s| {
                s.headers_named("authorization")
                    .first()
                    .map(|v| (*v).to_owned())
            })
            .collect()
    }

    /// The stored LFS object `oid`, if one was uploaded.
    #[must_use]
    pub fn lfs_object(&self, oid: &str) -> Option<Vec<u8>> {
        lock(&self.state.objects).get(oid).cloned()
    }
}

/// A root, and a server configuration for a leaf signed by it for `name`.
fn tls_config(
    names: &[String],
    versions: Versions,
    valid: (i32, i32),
) -> (CertificateDer<'static>, Arc<rustls::ServerConfig>) {
    let ca_key = rcgen::KeyPair::generate().unwrap();
    let mut ca = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
    ca.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    ca.distinguished_name
        .push(rcgen::DnType::CommonName, "Puddle fake git host root");
    let ca_cert = ca.self_signed(&ca_key).unwrap();
    let issuer = rcgen::Issuer::new(ca, ca_key);
    let mut leaf = rcgen::CertificateParams::new(names.to_vec()).unwrap();
    leaf.distinguished_name.push(
        rcgen::DnType::CommonName,
        names.first().cloned().unwrap_or_default(),
    );
    leaf.not_before = rcgen::date_time_ymd(valid.0, 1, 1);
    leaf.not_after = rcgen::date_time_ymd(valid.1, 1, 1);
    let key = rcgen::KeyPair::generate().unwrap();
    let cert = leaf.signed_by(&key, &issuer).unwrap();
    let mut config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(vec![cert.der().clone()], PrivateKeyDer::from(key))
    .unwrap();
    config.alpn_protocols = match versions {
        Versions::Http1 => vec![b"http/1.1".to_vec()],
        Versions::Both => vec![b"h2".to_vec(), b"http/1.1".to_vec()],
    };
    (ca_cert.der().clone(), Arc::new(config))
}

async fn accept_loop(listener: TcpListener, acceptor: TlsAcceptor, state: Arc<State>) {
    while let Ok((tcp, _)) = listener.accept().await {
        let acceptor = acceptor.clone();
        let state = state.clone();
        tokio::spawn(async move {
            let Ok(tls) = acceptor.accept(tcp).await else {
                return;
            };
            let h2 = tls.get_ref().1.alpn_protocol() == Some(b"h2");
            let version = if h2 { "HTTP/2" } else { "HTTP/1.1" };
            let service = service_fn(move |request| {
                let state = state.clone();
                async move { Ok::<_, std::convert::Infallible>(handle(&state, request, version).await) }
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

type Reply = Response<Full<Bytes>>;

fn reply(status: u16, headers: &[(&str, &str)], body: impl Into<Bytes>) -> Reply {
    let mut builder = Response::builder().status(status);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    builder.body(Full::new(body.into())).unwrap()
}

async fn handle(state: &State, request: Request<Incoming>, version: &'static str) -> Reply {
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
        // HTTP/2's `:authority` is the one place a Host lives.
        headers.insert(0, ("host".to_owned(), authority.to_string()));
    }
    let target = parts
        .uri
        .path_and_query()
        .map_or_else(|| parts.uri.path().to_owned(), ToString::to_string);
    let method = parts.method.as_str().to_owned();
    let answer = answer(state, &method, &target, &headers, &body).await;
    lock(&state.seen).push(Seen {
        method,
        target,
        headers,
        version,
        status: answer.status().as_u16(),
        body: body.iter().copied().take(64 * 1024).collect(),
    });
    answer
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

/// The token an `Authorization` carries, if the host accepts it; the user it stands for.
fn authenticate(state: &State, authorization: Option<&str>) -> Option<String> {
    let value = authorization?;
    let token = if let Some(token) = value.strip_prefix("Bearer ") {
        token.to_owned()
    } else {
        let encoded = value.strip_prefix("Basic ")?;
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .ok()?;
        let text = String::from_utf8(decoded).ok()?;
        text.split_once(':')?.1.to_owned()
    };
    lock(&state.tokens)
        .contains(&token)
        .then(|| format!("user-of-{}", token.len()))
}

/// Splits `/owner/name.git/rest` into the repository (`owner/name`) and the rest.
fn split_repo(path: &str) -> Option<(String, &str)> {
    let rest = path.strip_prefix('/')?;
    let (owner, rest) = rest.split_once('/')?;
    let (name, tail) = rest.split_once(".git")?;
    if owner.is_empty() || name.is_empty() || name.contains('/') {
        return None;
    }
    Some((format!("{owner}/{name}"), tail))
}

async fn answer(
    state: &State,
    method: &str,
    target: &str,
    headers: &[(String, String)],
    body: &[u8],
) -> Reply {
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let Some((repo, tail)) = split_repo(path) else {
        return reply(
            404,
            &[("content-type", "text/plain")],
            "not a repository path",
        );
    };
    let write = query.contains("service=git-receive-pack")
        || tail.ends_with("/git-receive-pack")
        || (tail.contains("/info/lfs/objects/") && method == "PUT")
        || (tail.ends_with("/info/lfs/objects/batch") && lfs_operation(body) == "upload");
    if repo.ends_with("/moved") {
        // A repository that has moved: the redirect points at another host.
        return reply(
            302,
            &[
                ("location", "https://evil.test/acme/web.git/info/refs"),
                ("content-type", "text/plain"),
            ],
            "moved",
        );
    }
    let private = lock(&state.private).contains(&repo);
    let user = authenticate(state, header(headers, "authorization"));
    if (private || write) && user.is_none() {
        return reply(
            401,
            &[
                ("www-authenticate", "Basic realm=\"fake git host\""),
                ("content-type", "text/plain"),
            ],
            "Authentication required",
        );
    }
    if let Some(oid) = tail.strip_prefix("/info/lfs/objects/") {
        return lfs(state, method, &repo, oid, body);
    }
    backend(state, method, path, query, headers, body, user.as_deref()).await
}

fn lfs_operation(body: &[u8]) -> String {
    serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|v| {
            v.get("operation")
                .and_then(|o| o.as_str().map(str::to_owned))
        })
        .unwrap_or_default()
}

fn lfs(state: &State, method: &str, repo: &str, oid: &str, body: &[u8]) -> Reply {
    match (method, oid) {
        ("POST", "batch") => {
            let value: serde_json::Value = serde_json::from_slice(body).unwrap_or_default();
            let operation = value
                .get("operation")
                .and_then(|o| o.as_str())
                .unwrap_or("download");
            let objects: Vec<serde_json::Value> = value
                .get("objects")
                .and_then(|o| o.as_array())
                .cloned()
                .unwrap_or_default()
                .iter()
                .map(|object| {
                    let oid = object.get("oid").and_then(|o| o.as_str()).unwrap_or_default();
                    let size = object.get("size").cloned().unwrap_or_default();
                    let href = format!("https://{}/{repo}.git/info/lfs/objects/{oid}", state.name);
                    let mut actions = BTreeMap::new();
                    actions.insert(operation, serde_json::json!({ "href": href, "header": {} }));
                    serde_json::json!({ "oid": oid, "size": size, "authenticated": true, "actions": actions })
                })
                .collect();
            let out = serde_json::json!({ "transfer": "basic", "objects": objects });
            reply(
                200,
                &[("content-type", "application/vnd.git-lfs+json")],
                out.to_string(),
            )
        }
        ("PUT", oid) => {
            lock(&state.objects).insert(oid.to_owned(), body.to_vec());
            reply(200, &[("content-type", "text/plain")], "stored")
        }
        ("GET", oid) => match lock(&state.objects).get(oid) {
            Some(object) => reply(
                200,
                &[("content-type", "application/octet-stream")],
                object.clone(),
            ),
            None => reply(404, &[("content-type", "text/plain")], "no such object"),
        },
        _ => reply(405, &[("content-type", "text/plain")], "method not allowed"),
    }
}

/// Runs `git http-backend` as CGI for one request.
async fn backend(
    state: &State,
    method: &str,
    path: &str,
    query: &str,
    headers: &[(String, String)],
    body: &[u8],
    user: Option<&str>,
) -> Reply {
    let mut body = body.to_vec();
    if header(headers, "content-encoding") == Some("gzip") {
        use std::io::Read as _;
        let mut plain = Vec::new();
        if flate2::read::GzDecoder::new(body.as_slice())
            .read_to_end(&mut plain)
            .is_err()
        {
            return reply(400, &[], "bad gzip");
        }
        body = plain;
    }
    let mut command = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    command
        .arg("http-backend")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_PROJECT_ROOT", &state.repos)
        .env("GIT_HTTP_EXPORT_ALL", "1")
        .env("REQUEST_METHOD", method)
        .env("PATH_INFO", path)
        .env("QUERY_STRING", query)
        .env("CONTENT_LENGTH", body.len().to_string())
        .env("REMOTE_ADDR", "127.0.0.1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(content_type) = header(headers, "content-type") {
        command.env("CONTENT_TYPE", content_type);
    }
    if let Some(protocol) = header(headers, "git-protocol") {
        command.env("GIT_PROTOCOL", protocol);
    }
    if let Some(user) = user {
        command.env("REMOTE_USER", user);
    }
    let mut child = command.spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let feed = async move {
        let _ = stdin.write_all(&body).await;
        drop(stdin);
    };
    let read = async move {
        let mut out = Vec::new();
        let _ = stdout.read_to_end(&mut out).await;
        out
    };
    let ((), out) = tokio::join!(feed, read);
    let _ = child.wait().await;
    cgi_response(&out)
}

/// A CGI document (header lines, a blank line, the body) as a response.
fn cgi_response(out: &[u8]) -> Reply {
    let split = out
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|at| (at, 4))
        .or_else(|| out.windows(2).position(|w| w == b"\n\n").map(|at| (at, 2)));
    let Some((at, gap)) = split else {
        return reply(500, &[("content-type", "text/plain")], "no CGI headers");
    };
    let head = String::from_utf8_lossy(out.get(..at).unwrap_or_default()).into_owned();
    let body = out.get(at + gap..).unwrap_or_default().to_vec();
    let mut status = 200;
    let mut builder = Response::builder();
    for line in head.lines() {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        if name.eq_ignore_ascii_case("status") {
            status = value
                .split(' ')
                .next()
                .and_then(|s| s.parse().ok())
                .unwrap_or(200);
        } else {
            builder = builder.header(name.trim(), value);
        }
    }
    builder
        .status(status)
        .body(Full::new(Bytes::from(body)))
        .unwrap()
}

/// A tiny HTTP `CONNECT` proxy that sends each tunnel to the address registered for the name in
/// its target: the company proxy that is the only one who knows the fake hosts' names. Any other
/// name is connected to as it stands. Stops when dropped.
pub struct NameProxy {
    port: u16,
    connects: Arc<Mutex<Vec<String>>>,
    task: JoinHandle<()>,
}

impl Drop for NameProxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl NameProxy {
    /// Starts a proxy that sends tunnels to `names` (`name`, address) to those addresses.
    pub async fn start(names: &[(&str, SocketAddr)]) -> Self {
        let names: Vec<(String, SocketAddr)> =
            names.iter().map(|(n, a)| ((*n).to_owned(), *a)).collect();
        let listener = TcpListener::bind((LOCAL, 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let connects = Arc::new(Mutex::new(Vec::new()));
        let seen = connects.clone();
        let task = tokio::spawn(async move {
            while let Ok((mut client, _)) = listener.accept().await {
                let seen = seen.clone();
                let names = names.clone();
                tokio::spawn(async move {
                    let mut head = Vec::new();
                    let mut byte = [0_u8; 1];
                    while !head.ends_with(b"\r\n\r\n") {
                        if client.read_exact(&mut byte).await.is_err() {
                            return;
                        }
                        head.extend_from_slice(&byte);
                    }
                    let text = String::from_utf8_lossy(&head).into_owned();
                    let authority = text
                        .split_whitespace()
                        .nth(1)
                        .unwrap_or_default()
                        .to_owned();
                    lock(&seen).push(authority.clone());
                    let wanted = authority.split(':').next().unwrap_or_default();
                    let target = names.iter().find(|(n, _)| n == wanted).map(|(_, a)| *a);
                    // A name the fake internet does not hold goes where it says, as through any
                    // open company proxy (the image pull goes through the same chain).
                    let connected = match target {
                        Some(target) => TcpStream::connect(target).await,
                        None => TcpStream::connect(authority.as_str()).await,
                    };
                    let Ok(mut upstream) = connected else {
                        let _ = client.write_all(b"HTTP/1.1 502 Bad Gateway\r\n\r\n").await;
                        return;
                    };
                    let _ = client
                        .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
                        .await;
                    let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
                });
            }
        });
        Self {
            port,
            connects,
            task,
        }
    }

    /// The port the proxy listens on (on `127.0.0.1`).
    #[must_use]
    pub fn port(&self) -> u16 {
        self.port
    }

    /// The `CONNECT` targets seen so far (`host:port`).
    #[must_use]
    pub fn connects(&self) -> Vec<String> {
        lock(&self.connects).clone()
    }
}
