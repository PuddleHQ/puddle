// SPDX-License-Identifier: GPL-3.0-or-later
//! The development certificate and its private key, as handed to the boot plan.

use std::fmt;

use zeroize::Zeroizing;

/// A private key, PKCS#8 DER. It is wiped from memory on drop, never printed (`Debug` shows no
/// bytes) and has no `Clone`, `Display` or serde impls, so the only way to see it is
/// [`pkcs8_der`](Self::pkcs8_der) or [`to_pem`](Self::to_pem).
pub struct PrivateKey(Zeroizing<Vec<u8>>);

impl PrivateKey {
    /// Wraps PKCS#8 DER bytes.
    #[must_use]
    pub fn from_pkcs8_der(der: Vec<u8>) -> Self {
        Self(Zeroizing::new(der))
    }

    /// The key as PKCS#8 DER.
    #[must_use]
    pub fn pkcs8_der(&self) -> &[u8] {
        &self.0
    }

    /// The key as a `PRIVATE KEY` PEM block (what Vite, Angular and Next.js read).
    #[must_use]
    pub fn to_pem(&self) -> Zeroizing<String> {
        Zeroizing::new(pem::encode(&pem::Pem::new("PRIVATE KEY", self.0.to_vec())))
    }
}

impl fmt::Debug for PrivateKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PrivateKey(..)")
    }
}

/// Where the certificate in use came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Origin {
    /// The ASP.NET development certificate Windows already trusts.
    ReusedAspNet,
    /// Puddle's own localhost leaf, kept in the data folder.
    Generated,
}

/// The certificate every workspace gets at boot and its key. The key is the leaf's own: this
/// type never holds a certificate authority key.
#[derive(Debug)]
pub struct DevCertMaterial {
    /// Where it came from.
    pub origin: Origin,
    cert_der: Vec<u8>,
    key: PrivateKey,
}

impl DevCertMaterial {
    pub(crate) fn new(origin: Origin, cert_der: Vec<u8>, key: PrivateKey) -> Self {
        Self {
            origin,
            cert_der,
            key,
        }
    }

    /// The certificate, DER.
    #[must_use]
    pub fn cert_der(&self) -> &[u8] {
        &self.cert_der
    }

    /// The certificate as a `CERTIFICATE` PEM block.
    #[must_use]
    pub fn cert_pem(&self) -> String {
        pem::encode(&pem::Pem::new("CERTIFICATE", self.cert_der.clone()))
    }

    /// The private key.
    #[must_use]
    pub fn key(&self) -> &PrivateKey {
        &self.key
    }
}
