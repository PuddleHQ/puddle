// SPDX-License-Identifier: GPL-3.0-or-later
//! Reading why a handshake with the guest ended, so that a client that refuses Puddle's
//! certificate is told apart from one that merely went away.

use std::io;

use rustls::AlertDescription;

/// The certificate alert the guest's client sent, when that is why the handshake failed: it does
/// not trust the workspace's CA (the usual cause: a tool with its own list of roots), or found
/// something else wrong with the certificate. `None` for every other end: the client closed,
/// stalled or reset, or the failure was Puddle's own.
#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "rustls's AlertDescription lists every TLS alert; only the certificate ones are named and the rest are not a certificate rejection"
)]
pub(crate) fn rejected_certificate(err: &io::Error) -> Option<&'static str> {
    let tls = err.get_ref()?.downcast_ref::<rustls::Error>()?;
    let rustls::Error::AlertReceived(alert) = tls else {
        return None;
    };
    match alert {
        AlertDescription::UnknownCA => Some("unknown_ca"),
        AlertDescription::BadCertificate => Some("bad_certificate"),
        AlertDescription::CertificateUnknown => Some("certificate_unknown"),
        AlertDescription::UnsupportedCertificate => Some("unsupported_certificate"),
        AlertDescription::CertificateExpired => Some("certificate_expired"),
        AlertDescription::CertificateRevoked => Some("certificate_revoked"),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
