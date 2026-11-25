// SPDX-License-Identifier: AGPL-3.0-or-later
//! A GitHub Enterprise Server stand-in for the real `gh`: the few answers `gh auth login` and
//! `gh auth status` read, over TLS on port 443 of `localhost`.
//!
//! `gh` sends its token only to a host written without a port, so the stand-in is on 443. A token
//! is `madeup-good-<login>` and belongs to `<login>`; the stand-in knows no other account. A login
//! can be revoked (the next request with its token is a `401`), and the device flow answers with a
//! fixed code until the test approves it.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code: a panic is how a test fails"
)]

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, PoisonError};

use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

pub(crate) const HOST: &str = "localhost";
pub(crate) const USER_CODE: &str = "ABCD-1234";
const TOKEN_PREFIX: &str = "madeup-good-";

/// The made-up token of `login`.
pub(crate) fn token_of(login: &str) -> String {
    format!("{TOKEN_PREFIX}{login}")
}

#[derive(Default)]
struct State {
    revoked: BTreeSet<String>,
    /// The login the device flow will hand a token to, once the test approves it.
    approved: Option<String>,
    /// `METHOD path` of every request, for what `gh` asked.
    requests: Vec<String>,
}

pub(crate) struct StandIn {
    state: Arc<Mutex<State>>,
    /// The certificate to trust, as PEM.
    #[cfg_attr(
        not(windows),
        expect(
            dead_code,
            reason = "only the Windows branch of `Trusted` writes it to the store"
        )
    )]
    pub(crate) cert_pem: String,
    /// The common name of the certificate when the stand-in made it, so the caller can make the
    /// computer trust it and remove it again; `None` when a runner prepared and trusts it.
    pub(crate) generated_cn: Option<String>,
    _task: tokio::task::JoinHandle<()>,
}

/// The certificate and key the stand-in serves: from files a runner prepared (`PUDDLE_STANDIN_CERT_PEM`
/// and `PUDDLE_STANDIN_KEY_PEM`, for a computer where the test cannot trust a certificate itself), else new.
fn pki() -> (String, String, Option<String>) {
    if let (Some(cert), Some(key)) = (
        std::env::var_os("PUDDLE_STANDIN_CERT_PEM"),
        std::env::var_os("PUDDLE_STANDIN_KEY_PEM"),
    ) {
        return (
            std::fs::read_to_string(cert).unwrap(),
            std::fs::read_to_string(key).unwrap(),
            None,
        );
    }
    let key = rcgen::KeyPair::generate().unwrap();
    let mut params = rcgen::CertificateParams::new(vec![HOST.to_owned()]).unwrap();
    let mut name = [0_u8; 6];
    getrandom::fill(&mut name).unwrap();
    let cn = format!(
        "Puddle real-tools stand-in {}",
        puddle_types::hex_lower(&name)
    );
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, cn.clone());
    let cert = params.self_signed(&key).unwrap();
    (cert.pem(), key.serialize_pem(), Some(cn))
}

impl StandIn {
    /// Starts serving on port 443.
    pub(crate) async fn start() -> Self {
        let (cert_pem, key_pem, generated_cn) = pki();
        let cert: CertificateDer<'static> = cert_der(&cert_pem);
        let key = PrivateKeyDer::try_from(pem_body(&key_pem, "PRIVATE KEY")).unwrap();
        let config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)
        .unwrap();
        let acceptor = TlsAcceptor::from(Arc::new(config));
        // flake-ok: gh sends its token only to a host written without a port, so the stand-in must be on 443
        let listener = TcpListener::bind("127.0.0.1:443")
            .await
            .expect("the stand-in needs port 443 of 127.0.0.1 (gh sends its token only to a host without a port)");
        let state: Arc<Mutex<State>> = Arc::default();
        let shared = Arc::clone(&state);
        let task = tokio::spawn(async move {
            loop {
                let Ok((tcp, _)) = listener.accept().await else {
                    return;
                };
                let (acceptor, state) = (acceptor.clone(), Arc::clone(&shared));
                tokio::spawn(async move {
                    if let Ok(tls) = acceptor.accept(tcp).await {
                        serve(tls, state).await;
                    }
                });
            }
        });
        Self {
            state,
            cert_pem,
            generated_cn,
            _task: task,
        }
    }

    pub(crate) fn revoke(&self, login: &str) {
        self.state().revoked.insert(login.to_owned());
    }

    /// Lets the device flow finish for `login`.
    pub(crate) fn approve(&self, login: &str) {
        self.state().approved = Some(login.to_owned());
    }

    /// `METHOD path` of every request so far.
    pub(crate) fn requests(&self) -> Vec<String> {
        self.state().requests.clone()
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn pem_body(pem: &str, label: &str) -> Vec<u8> {
    let mut in_block = false;
    let mut text = String::new();
    for line in pem.lines() {
        if line.starts_with("-----BEGIN") && line.contains(label) {
            in_block = true;
        } else if line.starts_with("-----END") {
            in_block = false;
        } else if in_block {
            text.push_str(line.trim());
        }
    }
    decode_base64(&text)
}

fn cert_der(pem: &str) -> CertificateDer<'static> {
    CertificateDer::from(pem_body(pem, "CERTIFICATE"))
}

fn decode_base64(text: &str) -> Vec<u8> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::new();
    let (mut bits, mut have) = (0_u32, 0_u32);
    for byte in text.bytes().filter(|b| *b != b'=') {
        let value = ALPHABET.iter().position(|a| *a == byte).unwrap();
        bits = (bits << 6) | u32::try_from(value).unwrap();
        have += 6;
        if have >= 8 {
            have -= 8;
            out.push(u8::try_from((bits >> have) & 0xff).unwrap());
        }
    }
    out
}

struct Request {
    method: String,
    path: String,
    token: Option<String>,
    form: String,
}

async fn read_request<S: AsyncReadExt + Unpin>(stream: &mut S) -> Option<Request> {
    let mut buf = Vec::new();
    let head_end = loop {
        if let Some(at) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break at + 4;
        }
        let mut chunk = [0_u8; 4096];
        let n = stream.read(&mut chunk).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let mut lines = head.lines();
    let mut first = lines.next()?.split(' ');
    let (method, path) = (first.next()?.to_owned(), first.next()?.to_owned());
    let (mut length, mut token) = (0_usize, None);
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        if name.eq_ignore_ascii_case("content-length") {
            length = value.parse().ok()?;
        } else if name.eq_ignore_ascii_case("authorization") {
            token = value.split(' ').next_back().map(str::to_owned);
        }
    }
    while buf.len() < head_end + length {
        let mut chunk = [0_u8; 4096];
        let n = stream.read(&mut chunk).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    Some(Request {
        method,
        path,
        token,
        form: String::from_utf8_lossy(&buf[head_end..head_end + length]).into_owned(),
    })
}

fn answer(status: &str, body: &str, head_only: bool) -> Vec<u8> {
    let mut out = format!(
        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\nx-oauth-scopes: repo, read:org\r\ncontent-length: {}\r\n\r\n",
        body.len()
    )
    .into_bytes();
    if !head_only {
        out.extend_from_slice(body.as_bytes());
    }
    out
}

async fn serve<S: AsyncReadExt + AsyncWriteExt + Unpin>(mut stream: S, state: Arc<Mutex<State>>) {
    while let Some(request) = read_request(&mut stream).await {
        let reply = {
            let mut state = state.lock().unwrap_or_else(PoisonError::into_inner);
            state
                .requests
                .push(format!("{} {}", request.method, request.path));
            route(&request, &state)
        };
        if stream.write_all(&reply).await.is_err() {
            return;
        }
    }
}

fn route(request: &Request, state: &State) -> Vec<u8> {
    let head_only = request.method == "HEAD";
    let login = request
        .token
        .as_deref()
        .and_then(|t| t.strip_prefix(TOKEN_PREFIX))
        .filter(|login| !state.revoked.contains(*login));
    match (request.method.as_str(), request.path.as_str()) {
        ("POST", "/login/device/code") => answer(
            "200 OK",
            &format!(
                r#"{{"device_code":"dev-1","user_code":"{USER_CODE}","verification_uri":"https://{HOST}/login/device","expires_in":900,"interval":1}}"#
            ),
            false,
        ),
        ("POST", "/login/oauth/access_token") => {
            debug_assert!(request.form.contains("device_code"));
            match &state.approved {
                Some(login) => answer(
                    "200 OK",
                    &format!(
                        r#"{{"access_token":"{}","token_type":"bearer","scope":"repo,read:org"}}"#,
                        token_of(login)
                    ),
                    false,
                ),
                None => answer("200 OK", r#"{"error":"authorization_pending"}"#, false),
            }
        }
        (_, _) if login.is_none() => answer(
            "401 Unauthorized",
            r#"{"message":"Bad credentials"}"#,
            head_only,
        ),
        ("POST", "/api/graphql") => answer(
            "200 OK",
            &format!(
                r#"{{"data":{{"viewer":{{"login":"{}"}}}}}}"#,
                login.unwrap_or_default()
            ),
            false,
        ),
        ("GET" | "HEAD", "/api/v3/" | "/api/v3/user") => answer(
            "200 OK",
            &format!(r#"{{"login":"{}","id":1}}"#, login.unwrap_or_default()),
            head_only,
        ),
        _ => answer("404 Not Found", r#"{"message":"Not Found"}"#, head_only),
    }
}
