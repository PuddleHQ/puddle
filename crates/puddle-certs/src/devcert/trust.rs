// SPDX-License-Identifier: GPL-3.0-or-later
//! The host operations the development certificate needs, behind one trait so the choices made
//! on top of them are tested over a fake store and a Linux host builds without any of them.

use super::material::PrivateKey;

/// Whether the key of an existing certificate can be had.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum KeyAccess {
    /// The user has no private key for the certificate.
    Missing,
    /// There is a key, but it cannot be exported in plain form, and why.
    NotExportable(String),
    /// The key can be exported.
    Exportable,
}

/// A certificate the user already has that might serve as the development certificate: found in
/// their personal store with the ASP.NET marker or not (the choice looks at the marker).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExistingCert {
    /// The certificate, DER.
    pub der: Vec<u8>,
    /// What can be done with its key.
    pub key: KeyAccess,
    /// Whether the system already trusts it (it is in the user's trusted roots).
    pub trusted: bool,
}

/// What came of asking the user to trust a certificate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TrustOutcome {
    /// It is trusted now (or already was).
    Trusted,
    /// The user answered "No" to the system's confirmation.
    Declined,
}

/// Why a host operation failed. Messages carry OS error codes, never key material.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum TrustError {
    /// This host has no implementation yet.
    #[error("development certificate trust is not supported on this system yet")]
    Unsupported,
    /// The operation failed.
    #[error("{0}")]
    Failed(String),
}

/// The development certificate's host side. Windows implements it over the current
/// user's certificate stores; Linux has [`Unsupported`], and the NSS implementation is later work.
pub trait DevCertTrust: Send + Sync {
    /// The certificates in the user's personal store that have a private key, with what is known
    /// about the key and the trust of each.
    ///
    /// # Errors
    ///
    /// [`TrustError::Unsupported`] on a host without an implementation; otherwise the store
    /// could not be read.
    fn existing(&self) -> Result<Vec<ExistingCert>, TrustError>;

    /// The private key of `cert_der`, from the user's personal store.
    ///
    /// # Errors
    ///
    /// The key is not there or may not be exported.
    fn export_key(&self, cert_der: &[u8]) -> Result<PrivateKey, TrustError>;

    /// Whether the system trusts `cert_der` as a root for the current user.
    ///
    /// # Errors
    ///
    /// The store could not be read.
    fn is_trusted(&self, cert_der: &[u8]) -> Result<bool, TrustError>;

    /// Asks the system to trust `cert_der` for the current user. The system confirms with the
    /// user, once; a policy that forbids the addition is never worked around.
    ///
    /// # Errors
    ///
    /// Anything but the user declining (that is [`TrustOutcome::Declined`]).
    fn trust(&self, cert_der: &[u8]) -> Result<TrustOutcome, TrustError>;
}

/// The host without an implementation: every call says so.
#[derive(Debug, Default, Clone, Copy)]
pub struct Unsupported;

impl DevCertTrust for Unsupported {
    fn existing(&self) -> Result<Vec<ExistingCert>, TrustError> {
        Err(TrustError::Unsupported)
    }
    fn export_key(&self, _cert_der: &[u8]) -> Result<PrivateKey, TrustError> {
        Err(TrustError::Unsupported)
    }
    fn is_trusted(&self, _cert_der: &[u8]) -> Result<bool, TrustError> {
        Err(TrustError::Unsupported)
    }
    fn trust(&self, _cert_der: &[u8]) -> Result<TrustOutcome, TrustError> {
        Err(TrustError::Unsupported)
    }
}
