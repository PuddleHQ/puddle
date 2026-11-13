// SPDX-License-Identifier: AGPL-3.0-or-later
//! TLS from the host to a real server: Puddle's side of a terminated connection, and of its own
//! requests.
//!
//! The server's certificate is checked the way the host's own programs check it, so a host on a
//! company network that re-signs traffic works when its root is trusted:
//!
//! - **Windows**: the platform verifier (`CryptoAPI`), which already honours the machine's and
//!   the user's root stores, plus any extra roots the caller passes;
//! - **elsewhere**: the system's roots plus the extra roots, checked by `WebPKI`.
//!
//! Nothing the guest says reaches the check: the name is the one the host already admitted and
//! the clock is the host's. Session resumption is off, so no TLS state is shared between
//! connections (and so between workspaces).

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use rustls::client::{ResolvesClientCert, Resumption};
use rustls::pki_types::{CertificateDer, ServerName};
use rustls::{CertificateError, ClientConfig, RootCertStore};
use rustls_platform_verifier::Verifier;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;

/// The protocol offered when the caller does not choose: HTTP/1.1.
const ALPN_HTTP11: &[u8] = b"http/1.1";

/// Why the verifier could not be built.
#[derive(Debug, thiserror::Error)]
#[error("could not set up certificate verification: {0}")]
pub struct TlsSetupError(String);

/// Why a TLS connection to a server was not made.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TlsConnectError {
    /// The name is not a valid DNS name (an IP literal is refused too: Puddle verifies names).
    #[error("{0:?} is not a valid server name")]
    InvalidName(String),
    /// The server's certificate was not accepted: unknown issuer, expired, wrong name, ... For
    /// the common reasons the text is one fixed sentence on every platform, led by the name rustls
    /// gives the reason; for the rest it is the verifier's own text.
    #[error("{0}")]
    Certificate(String),
    /// Any other handshake or transport failure.
    #[error("{0}")]
    Handshake(String),
}

impl TlsConnectError {
    fn from_io(err: &io::Error) -> Self {
        let tls = err
            .get_ref()
            .and_then(|inner| inner.downcast_ref::<rustls::Error>());
        match tls {
            Some(rustls::Error::InvalidCertificate(why)) => {
                Self::Certificate(certificate_reason(why))
            }
            Some(other) => Self::Handshake(other.to_string()),
            None => Self::Handshake(err.to_string()),
        }
    }
}

/// One wording for a refused certificate, whichever platform verifier refused it. The verifiers
/// word the same reason differently ("certificate expired: verification time ..." through webpki,
/// a bare "Expired" from the Windows chain engine), so the common ones get a fixed sentence and
/// the name rustls gives the reason; anything else keeps the verifier's own text, which may be
/// all there is to go on.
fn certificate_reason(why: &CertificateError) -> String {
    use CertificateError as C;
    let (code, what) = match why {
        C::Expired | C::ExpiredContext { .. } => ("Expired", "the certificate has expired"),
        C::NotValidYet | C::NotValidYetContext { .. } => (
            "NotValidYet",
            "the certificate is not valid yet; check that this computer's clock is right",
        ),
        C::UnknownIssuer => (
            "UnknownIssuer",
            "the certificate was issued by an authority this computer does not trust",
        ),
        C::NotValidForName | C::NotValidForNameContext { .. } => (
            "NotValidForName",
            "the certificate is for a different name than the one asked for",
        ),
        C::Revoked => ("Revoked", "the certificate was revoked by its issuer"),
        C::BadSignature => (
            "BadSignature",
            "the certificate has a signature that does not check out",
        ),
        _ => return rustls::Error::InvalidCertificate(why.clone()).to_string(),
    };
    format!("invalid peer certificate: {code} ({what})")
}

/// An extra root certificate [`TlsClient::new`] could not use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RejectedRoot {
    /// The certificate as it was passed in, so the caller can name it (subject, fingerprint).
    pub der: CertificateDer<'static>,
    /// Why the verifier refused it, in rustls's words.
    pub reason: String,
}

/// A TLS client with Puddle's verification policy. Cheap to clone; build one per process.
#[derive(Clone)]
pub struct TlsClient {
    config: Arc<ClientConfig>,
    rejected: Arc<[RejectedRoot]>,
}

impl std::fmt::Debug for TlsClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TlsClient").finish_non_exhaustive()
    }
}

impl TlsClient {
    /// A client that trusts what the platform trusts plus `extra_roots` (the corporate roots
    /// Puddle syncs into the guest, and any the caller adds). A root the verifier cannot use is
    /// left out, with a warning, rather than failing every connection: it is listed in
    /// [`TlsClient::rejected_roots`], and a site that chains to it fails with an unknown issuer.
    ///
    /// # Errors
    /// [`TlsSetupError`] when the platform's trust store cannot be read at all.
    pub fn new(
        extra_roots: impl IntoIterator<Item = CertificateDer<'static>>,
    ) -> Result<Self, TlsSetupError> {
        Self::build(extra_roots, None)
    }

    /// As [`Self::new`], with the verification clock fixed at `now` (tests of the clock rule).
    ///
    /// # Errors
    /// As [`Self::new`].
    #[cfg(any(test, feature = "testing"))]
    pub fn with_clock(
        extra_roots: impl IntoIterator<Item = CertificateDer<'static>>,
        now: std::time::SystemTime,
    ) -> Result<Self, TlsSetupError> {
        Self::build(extra_roots, Some(Arc::new(FixedClock(now))))
    }

    fn build(
        extra_roots: impl IntoIterator<Item = CertificateDer<'static>>,
        clock: Option<Arc<dyn rustls::time_provider::TimeProvider>>,
    ) -> Result<Self, TlsSetupError> {
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut kept = Vec::new();
        let mut rejected = Vec::new();
        for root in extra_roots {
            match RootCertStore::empty().add(root.clone()) {
                Ok(()) => kept.push(root),
                Err(err) => rejected.push(RejectedRoot {
                    der: root,
                    reason: err.to_string(),
                }),
            }
        }
        if !rejected.is_empty() {
            let reasons: Vec<&str> = rejected.iter().map(|r| r.reason.as_str()).collect();
            tracing::warn!(
                rejected = rejected.len(),
                reasons = %reasons.join("; "),
                "extra root certificates the verifier cannot use were left out; servers that chain to them will fail with an unknown issuer"
            );
        }
        let verifier = Verifier::new_with_extra_roots(kept, Arc::clone(&provider))
            .map_err(|err| TlsSetupError(err.to_string()))?;
        let builder = match clock {
            Some(clock) => ClientConfig::builder_with_details(Arc::clone(&provider), clock),
            None => ClientConfig::builder_with_provider(Arc::clone(&provider)),
        };
        let mut config = builder
            .with_safe_default_protocol_versions()
            .map_err(|err| TlsSetupError(err.to_string()))?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(verifier))
            .with_no_client_auth();
        config.alpn_protocols = vec![ALPN_HTTP11.to_vec()];
        config.resumption = Resumption::disabled();
        Ok(Self {
            config: Arc::new(config),
            rejected: rejected.into(),
        })
    }

    /// The extra roots [`TlsClient::new`] left out, and why. Empty when every one was used.
    #[must_use]
    pub fn rejected_roots(&self) -> &[RejectedRoot] {
        &self.rejected
    }

    /// Runs the handshake with `server_name` over `stream` (any connection already made: direct,
    /// or a tunnel through the company proxy) and returns the encrypted stream. Offers
    /// `http/1.1` only.
    ///
    /// # Errors
    /// [`TlsConnectError`]; the stream is dropped.
    pub async fn connect<S>(
        &self,
        server_name: &str,
        stream: S,
    ) -> Result<TlsStream<S>, TlsConnectError>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        self.connect_alpn(server_name, stream, &[ALPN_HTTP11]).await
    }

    /// As [`Self::connect`], offering `alpn` (in order of preference) instead of `http/1.1`. What
    /// the server chose is `stream.get_ref().1.alpn_protocol()` on the result.
    ///
    /// # Errors
    /// [`TlsConnectError`]; the stream is dropped.
    pub async fn connect_alpn<S>(
        &self,
        server_name: &str,
        stream: S,
        alpn: &[&[u8]],
    ) -> Result<TlsStream<S>, TlsConnectError>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        let name = dns_name(server_name)?;
        let config = if alpn == [ALPN_HTTP11] {
            Arc::clone(&self.config)
        } else {
            let mut config = ClientConfig::clone(&self.config);
            config.alpn_protocols = alpn.iter().map(|p| p.to_vec()).collect();
            Arc::new(config)
        };
        TlsConnector::from(config)
            .connect(name, stream)
            .await
            .map_err(|err| TlsConnectError::from_io(&err))
    }

    /// As [`Self::connect_alpn`], and says whether the server asked for a client certificate
    /// during the handshake. Puddle has none to give, so a server that asks (and most that ask
    /// then refuse the connection, in TLS 1.3 only after the handshake looked finished) cannot be
    /// talked to through Puddle's own TLS: the caller can pass the connection through instead.
    ///
    /// # Errors
    /// [`TlsConnectError`]; the stream is dropped.
    pub async fn connect_alpn_noting_client_auth<S>(
        &self,
        server_name: &str,
        stream: S,
        alpn: &[&[u8]],
    ) -> Result<(TlsStream<S>, bool), TlsConnectError>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        let name = dns_name(server_name)?;
        let asked = Arc::new(AskedForCertificate::default());
        let mut config = ClientConfig::clone(&self.config);
        config.alpn_protocols = alpn.iter().map(|p| p.to_vec()).collect();
        config.client_auth_cert_resolver = Arc::clone(&asked) as Arc<dyn ResolvesClientCert>;
        let tls = TlsConnector::from(Arc::new(config))
            .connect(name, stream)
            .await
            .map_err(|err| TlsConnectError::from_io(&err))?;
        Ok((tls, asked.0.load(Ordering::Relaxed)))
    }
}

fn dns_name(server_name: &str) -> Result<ServerName<'static>, TlsConnectError> {
    match ServerName::try_from(server_name.to_owned()) {
        Ok(name @ ServerName::DnsName(_)) => Ok(name),
        _ => Err(TlsConnectError::InvalidName(server_name.to_owned())),
    }
}

/// Offers no client certificate and remembers that the server asked for one.
#[derive(Debug, Default)]
struct AskedForCertificate(AtomicBool);

impl ResolvesClientCert for AskedForCertificate {
    fn resolve(
        &self,
        _root_hint_subjects: &[&[u8]],
        _sigschemes: &[rustls::SignatureScheme],
    ) -> Option<Arc<rustls::sign::CertifiedKey>> {
        self.0.store(true, Ordering::Relaxed);
        None
    }

    fn has_certs(&self) -> bool {
        false
    }
}

/// Connects `stream` to `server_name` over TLS with `client`: [`TlsClient::connect`] for Puddle's
/// own requests, after [`crate::host::connect`] made the connection.
///
/// # Errors
/// As [`TlsClient::connect`].
pub async fn tls_connect<S>(
    client: &TlsClient,
    server_name: &str,
    stream: S,
) -> Result<TlsStream<S>, TlsConnectError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    client.connect(server_name, stream).await
}

#[cfg(any(test, feature = "testing"))]
#[derive(Debug)]
struct FixedClock(std::time::SystemTime);

#[cfg(any(test, feature = "testing"))]
impl rustls::time_provider::TimeProvider for FixedClock {
    fn current_time(&self) -> Option<rustls::pki_types::UnixTime> {
        Some(rustls::pki_types::UnixTime::since_unix_epoch(
            self.0.duration_since(std::time::UNIX_EPOCH).ok()?,
        ))
    }
}

#[cfg(test)]
mod tests;
