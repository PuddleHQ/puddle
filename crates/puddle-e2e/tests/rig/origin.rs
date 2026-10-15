// SPDX-License-Identifier: GPL-3.0-or-later
//! Origins and clients for the corporate-lab tests: an echo server, a TLS origin with a root of
//! its own, and a TLS client that trusts exactly the roots it is given.
#![expect(
    clippy::unwrap_used,
    reason = "helpers outside #[test] functions fail the test by panicking"
)]
#![allow(dead_code, reason = "each test binary uses some of the pieces")]

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

pub(crate) const LOCAL: Ipv4Addr = Ipv4Addr::LOCALHOST;

/// An echo server on loopback.
pub(crate) async fn echo_server() -> SocketAddr {
    let echo = TcpListener::bind((LOCAL, 0)).await.unwrap();
    let echo_addr = echo.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (mut conn, _) = echo.accept().await.unwrap();
            tokio::spawn(async move {
                let (mut r, mut w) = conn.split();
                let _ = tokio::io::copy(&mut r, &mut w).await;
                let _ = w.shutdown().await;
            });
        }
    });
    echo_addr
}

/// A TLS origin on loopback whose certificate (for `name`) is signed by a root of its own; the
/// root is what a client trusts when nothing sits between it and the origin.
pub(crate) struct HttpsOrigin {
    pub(crate) port: u16,
    pub(crate) root_der: Vec<u8>,
}

impl HttpsOrigin {
    pub(crate) const BODY: &str = "hello from the origin";

    /// Serves `BODY` to every request, one request per connection.
    pub(crate) async fn start(name: &str) -> Self {
        use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
        let ca_key = rcgen::KeyPair::generate().unwrap();
        let mut ca_params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
        ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        ca_params
            .distinguished_name
            .push(rcgen::DnType::CommonName, "puddle lab origin root");
        let ca = ca_params.self_signed(&ca_key).unwrap();
        let issuer = rcgen::Issuer::new(ca_params, ca_key);
        let leaf_key = rcgen::KeyPair::generate().unwrap();
        let leaf = rcgen::CertificateParams::new(vec![name.to_owned()])
            .unwrap()
            .signed_by(&leaf_key, &issuer)
            .unwrap();
        let config = Arc::new(
            rustls::ServerConfig::builder_with_provider(Arc::new(
                rustls::crypto::ring::default_provider(),
            ))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from(leaf.der().to_vec())],
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(leaf_key.serialize_der())),
            )
            .unwrap(),
        );
        let listener = TcpListener::bind((LOCAL, 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let acceptor = tokio_rustls::TlsAcceptor::from(config);
        tokio::spawn(async move {
            loop {
                let (conn, _) = listener.accept().await.unwrap();
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    let Ok(tls) = acceptor.accept(conn).await else {
                        return;
                    };
                    let mut reader = BufReader::new(tls);
                    loop {
                        let mut l = String::new();
                        if reader.read_line(&mut l).await.unwrap_or(0) == 0 || l == "\r\n" {
                            break;
                        }
                    }
                    let response = format!(
                        "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                        Self::BODY.len(),
                        Self::BODY
                    );
                    let _ = reader.get_mut().write_all(response.as_bytes()).await;
                    let _ = reader.get_mut().shutdown().await;
                });
            }
        });
        Self {
            port,
            root_der: ca.der().to_vec(),
        }
    }
}

/// What a TLS client got over a tunnel: the handshake error, or the response body.
pub(crate) async fn https_get(
    tunnel: BufReader<TcpStream>,
    name: &str,
    trusted_roots: &[&[u8]],
) -> Result<String, String> {
    use rustls::pki_types::{CertificateDer, ServerName};
    let mut roots = rustls::RootCertStore::empty();
    for der in trusted_roots {
        roots.add(CertificateDer::from(der.to_vec())).unwrap();
    }
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_root_certificates(roots)
    .with_no_client_auth();
    let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
    let server_name = ServerName::try_from(name.to_owned()).unwrap();
    let mut tls = connector
        .connect(server_name, tunnel)
        .await
        .map_err(|e| format!("{e:?}"))?;
    tls.write_all(
        format!("GET / HTTP/1.1\r\nHost: {name}\r\nConnection: close\r\n\r\n").as_bytes(),
    )
    .await
    .unwrap();
    // A peer that closes without a TLS close_notify is an error after the data; keep the data.
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 4096];
    while let Ok(n) = tls.read(&mut chunk).await {
        if n == 0 {
            break;
        }
        bytes.extend_from_slice(chunk.get(..n).unwrap_or_default());
    }
    let response = String::from_utf8_lossy(&bytes).into_owned();
    Ok(response
        .split("\r\n\r\n")
        .nth(1)
        .unwrap_or_default()
        .to_owned())
}
