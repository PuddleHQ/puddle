// SPDX-License-Identifier: AGPL-3.0-or-later
//! The host operations the development certificate needs, behind one trait so the choices made
//! on top of them are tested over a fake store and a Linux host builds without any of them.

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
    /// Whether this host can trust a certificate at all: `false` only for [`Unsupported`], whose
    /// every call fails, so the status can say so before any certificate exists.
    fn is_supported(&self) -> bool {
        true
    }

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
    fn is_supported(&self) -> bool {
        false
    }
    fn is_trusted(&self, _cert_der: &[u8]) -> Result<bool, TrustError> {
        Err(TrustError::Unsupported)
    }
    fn trust(&self, _cert_der: &[u8]) -> Result<TrustOutcome, TrustError> {
        Err(TrustError::Unsupported)
    }
}
