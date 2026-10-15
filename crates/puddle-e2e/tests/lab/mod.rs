// SPDX-License-Identifier: GPL-3.0-or-later
//! The lab registry's shared pieces: its name, a resolver that points the name at a local
//! listener, the lab root and a server certificate signed by it, and the digest format.
#![expect(
    clippy::unwrap_used,
    reason = "helpers outside #[test] functions fail the test by panicking"
)]
#![allow(dead_code, reason = "each test binary uses some of the pieces")]

use std::fmt::Write as _;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;

use puddle_proxy::{BoxFuture, Resolver};
use puddle_types::DomainName;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use sha2::{Digest, Sha256};

/// The registry's name; only the pull proxy's resolver knows it.
pub(crate) const REGISTRY: &str = "registry.lab.test";

/// Resolves [`REGISTRY`] to the local lab registry, whatever port was asked for.
pub(crate) struct LabResolver(pub(crate) SocketAddr);

impl Resolver for LabResolver {
    fn resolve<'a>(
        &'a self,
        name: &'a DomainName,
        _port: u16,
    ) -> BoxFuture<'a, io::Result<Vec<SocketAddr>>> {
        let found = (name.as_str() == REGISTRY).then_some(vec![self.0]);
        Box::pin(async move {
            found.ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "unknown lab name"))
        })
    }
}

/// The lab root (PEM) and the registry's server config, its leaf signed by that root.
pub(crate) fn lab_pki() -> (String, rustls::ServerConfig) {
    let ca_key = rcgen::KeyPair::generate().unwrap();
    let mut ca_params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
    ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    ca_params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "Puddle lab interceptor root");
    let ca = ca_params.self_signed(&ca_key).unwrap();
    let issuer = rcgen::Issuer::new(ca_params, ca_key);
    let leaf_key = rcgen::KeyPair::generate().unwrap();
    let leaf = rcgen::CertificateParams::new(vec![REGISTRY.to_owned()])
        .unwrap()
        .signed_by(&leaf_key, &issuer)
        .unwrap();
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(
        vec![
            CertificateDer::from(leaf.der().to_vec()),
            CertificateDer::from(ca.der().to_vec()),
        ],
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(leaf_key.serialize_der())),
    )
    .unwrap();
    (ca.pem(), config)
}

pub(crate) fn sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().fold(String::from("sha256:"), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}
