// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

fn alert(description: AlertDescription) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        rustls::Error::AlertReceived(description),
    )
}

#[test]
fn a_certificate_alert_from_the_client_is_a_refused_certificate() {
    for (description, name) in [
        (AlertDescription::UnknownCA, "unknown_ca"),
        (AlertDescription::BadCertificate, "bad_certificate"),
        (AlertDescription::CertificateUnknown, "certificate_unknown"),
        (
            AlertDescription::UnsupportedCertificate,
            "unsupported_certificate",
        ),
        (AlertDescription::CertificateExpired, "certificate_expired"),
        (AlertDescription::CertificateRevoked, "certificate_revoked"),
    ] {
        assert_eq!(rejected_certificate(&alert(description)), Some(name));
    }
}

#[test]
fn any_other_end_of_a_handshake_is_not() {
    for other in [
        alert(AlertDescription::HandshakeFailure),
        alert(AlertDescription::ProtocolVersion),
        alert(AlertDescription::CloseNotify),
        io::Error::new(io::ErrorKind::UnexpectedEof, "tls handshake eof"),
        io::Error::from(io::ErrorKind::ConnectionReset),
        io::Error::new(
            io::ErrorKind::InvalidData,
            rustls::Error::General("no server certificate chain resolved".into()),
        ),
    ] {
        assert_eq!(rejected_certificate(&other), None, "{other}");
    }
}
