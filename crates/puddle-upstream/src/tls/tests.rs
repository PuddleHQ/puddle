// SPDX-License-Identifier: AGPL-3.0-or-later
use std::time::{Duration, SystemTime};

use rustls::pki_types::PrivateKeyDer;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

use super::*;

struct Server {
    port: u16,
    root: CertificateDer<'static>,
}

/// A TLS server with a leaf for `name` (SAN `san`) from a fresh root, valid `years` around
/// now; it answers every connection with `hello` and closes.
async fn server(san: &str, years: (i64, i64)) -> Server {
    server_asking(san, years, false).await
}

/// As [`server`]; with `ask_for_certificate` it also asks the client for a certificate (and
/// goes on without one, as a server with optional client authentication does).
async fn server_asking(san: &str, years: (i64, i64), ask_for_certificate: bool) -> Server {
    let ca_key = rcgen::KeyPair::generate().unwrap();
    let mut ca = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
    ca.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    ca.distinguished_name
        .push(rcgen::DnType::CommonName, "tls test root");
    let ca_cert = ca.self_signed(&ca_key).unwrap();
    let issuer = rcgen::Issuer::new(ca, ca_key);
    let key = rcgen::KeyPair::generate().unwrap();
    let mut leaf = rcgen::CertificateParams::new(vec![san.to_owned()]).unwrap();
    let now = time_now_year();
    leaf.not_before = rcgen::date_time_ymd(i32::try_from(now + years.0).unwrap(), 1, 1);
    leaf.not_after = rcgen::date_time_ymd(i32::try_from(now + years.1).unwrap(), 1, 1);
    let cert = leaf.signed_by(&key, &issuer).unwrap();
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let builder = rustls::ServerConfig::builder_with_provider(Arc::clone(&provider))
        .with_safe_default_protocol_versions()
        .unwrap();
    let builder = if ask_for_certificate {
        let mut roots = RootCertStore::empty();
        roots.add(ca_cert.der().clone()).unwrap();
        let verifier =
            rustls::server::WebPkiClientVerifier::builder_with_provider(Arc::new(roots), provider)
                .allow_unauthenticated()
                .build()
                .unwrap();
        builder.with_client_cert_verifier(verifier)
    } else {
        builder.with_no_client_auth()
    };
    let mut config = builder
        .with_single_cert(vec![cert.der().clone()], PrivateKeyDer::from(key))
        .unwrap();
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    let acceptor = TlsAcceptor::from(Arc::new(config));
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((tcp, _)) = listener.accept().await {
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                if let Ok(mut tls) = acceptor.accept(tcp).await {
                    let _ = tls.write_all(b"hello").await;
                    let _ = tls.shutdown().await;
                }
            });
        }
    });
    Server {
        port,
        root: ca_cert.der().clone(),
    }
}

fn time_now_year() -> i64 {
    let secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    1970 + i64::try_from(secs / 31_556_952).unwrap()
}

async fn dial(server: &Server, client: &TlsClient, name: &str) -> Result<String, TlsConnectError> {
    let tcp = tokio::net::TcpStream::connect(("127.0.0.1", server.port))
        .await
        .unwrap();
    let mut tls = tls_connect(client, name, tcp).await?;
    assert_eq!(tls.get_ref().1.alpn_protocol(), Some(&b"http/1.1"[..]));
    let mut out = String::new();
    let _ = tls.read_to_string(&mut out).await;
    Ok(out)
}

#[tokio::test]
async fn the_offered_protocols_decide_what_the_server_picks() {
    let server = server("host.test", (-1, 1)).await;
    let client = TlsClient::new([server.root.clone()]).unwrap();
    for (offer, picked) in [
        (vec![&b"h2"[..], b"http/1.1"], Some(&b"h2"[..])),
        (vec![b"http/1.1"], Some(b"http/1.1")),
        (vec![b"h2"], Some(b"h2")),
        (vec![b"spdy/3"], None),
    ] {
        let tcp = tokio::net::TcpStream::connect(("127.0.0.1", server.port))
            .await
            .unwrap();
        let tls = client.connect_alpn("host.test", tcp, &offer).await;
        // A server with no protocol in common ends the handshake, as real servers do.
        match picked {
            Some(picked) => {
                assert_eq!(tls.unwrap().get_ref().1.alpn_protocol(), Some(picked));
            }
            None => assert!(tls.is_err()),
        }
    }
}

#[tokio::test]
async fn a_server_that_asks_for_a_client_certificate_is_told_apart_from_one_that_does_not() {
    for (asks, name) in [(true, "asks.test"), (false, "plain.test")] {
        let server = server_asking(name, (-1, 1), asks).await;
        let client = TlsClient::new([server.root.clone()]).unwrap();
        let tcp = tokio::net::TcpStream::connect(("127.0.0.1", server.port))
            .await
            .unwrap();
        let (tls, asked) = client
            .connect_alpn_noting_client_auth(name, tcp, &[b"http/1.1"])
            .await
            .unwrap();
        assert_eq!(asked, asks, "{name}");
        assert_eq!(tls.get_ref().1.alpn_protocol(), Some(&b"http/1.1"[..]));
    }
}

#[tokio::test]
async fn a_certificate_from_a_trusted_extra_root_is_accepted() {
    let server = server("host.test", (-1, 1)).await;
    let client = TlsClient::new([server.root.clone()]).unwrap();
    assert_eq!(dial(&server, &client, "host.test").await.unwrap(), "hello");
}

#[tokio::test]
async fn a_root_nobody_trusts_is_refused_with_the_reason() {
    let server = server("host.test", (-1, 1)).await;
    let client = TlsClient::new([]).unwrap();
    let err = dial(&server, &client, "host.test").await.unwrap_err();
    assert!(
        matches!(&err, TlsConnectError::Certificate(r) if r.contains("UnknownIssuer")),
        "{err}"
    );
}

#[test]
fn every_reason_has_one_wording_and_the_rest_keep_the_verifiers_text() {
    use CertificateError as C;
    let now = rustls::pki_types::UnixTime::since_unix_epoch(Duration::from_secs(100));
    for (why, code) in [
        (C::Expired, "Expired"),
        (
            C::ExpiredContext {
                time: now,
                not_after: now,
            },
            "Expired",
        ),
        (C::NotValidYet, "NotValidYet"),
        (
            C::NotValidYetContext {
                time: now,
                not_before: now,
            },
            "NotValidYet",
        ),
        (C::UnknownIssuer, "UnknownIssuer"),
        (C::NotValidForName, "NotValidForName"),
        (C::Revoked, "Revoked"),
        (C::BadSignature, "BadSignature"),
    ] {
        let said = certificate_reason(&why);
        assert!(
            said.starts_with(&format!(
                "invalid peer certificate: {code} (the certificate "
            )),
            "the reason for {code} has the wrong shape"
        );
        assert!(
            !said.contains("verification time"),
            "the reason for {code} names the verification time"
        );
    }
    assert_eq!(
        certificate_reason(&C::BadEncoding),
        "invalid peer certificate: BadEncoding"
    );
}

#[tokio::test]
async fn the_wrong_name_and_the_wrong_time_are_refused() {
    let wrong_name = server("other.test", (-1, 1)).await;
    let client = TlsClient::new([wrong_name.root.clone()]).unwrap();
    let err = dial(&wrong_name, &client, "host.test").await.unwrap_err();
    assert!(matches!(err, TlsConnectError::Certificate(_)), "{err}");
    let expired = server("host.test", (-3, -2)).await;
    let client = TlsClient::new([expired.root.clone()]).unwrap();
    let err = dial(&expired, &client, "host.test").await.unwrap_err();
    // The platform verifiers word it differently; the user reads one sentence.
    assert_eq!(
        err,
        TlsConnectError::Certificate(
            "invalid peer certificate: Expired (the certificate has expired)".to_owned()
        )
    );
}

#[tokio::test]
async fn the_verification_clock_is_the_one_the_client_was_built_with() {
    let server = server("host.test", (-1, 1)).await;
    let far_future = SystemTime::now() + Duration::from_hours(20 * 365 * 24);
    let client = TlsClient::with_clock([server.root.clone()], far_future).unwrap();
    let err = dial(&server, &client, "host.test").await.unwrap_err();
    assert!(matches!(err, TlsConnectError::Certificate(_)), "{err}");
    let client = TlsClient::with_clock([server.root.clone()], SystemTime::now()).unwrap();
    assert_eq!(dial(&server, &client, "host.test").await.unwrap(), "hello");
}

#[tokio::test]
async fn only_dns_names_are_verified() {
    let server = server("host.test", (-1, 1)).await;
    let client = TlsClient::new([server.root.clone()]).unwrap();
    for name in ["127.0.0.1", "::1", "", "not a name", "bad_\u{0}name"] {
        let tcp = tokio::net::TcpStream::connect(("127.0.0.1", server.port))
            .await
            .unwrap();
        let err = client.connect(name, tcp).await.unwrap_err();
        assert_eq!(
            err,
            TlsConnectError::InvalidName(name.to_owned()),
            "{name:?}"
        );
    }
}

#[tokio::test]
async fn a_server_that_is_not_tls_is_a_handshake_error() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        if let Ok((mut tcp, _)) = listener.accept().await {
            let _ = tcp.write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n").await;
        }
    });
    let client = TlsClient::new([]).unwrap();
    let tcp = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    let err = client.connect("host.test", tcp).await.unwrap_err();
    assert!(matches!(err, TlsConnectError::Handshake(_)), "{err}");
}

#[test]
fn roots_that_do_not_parse_are_skipped_and_listed_with_the_reason() {
    let junk = CertificateDer::from(vec![0_u8, 1, 2, 3]);
    let client = TlsClient::new([junk.clone()]).unwrap();
    let rejected = client.rejected_roots();
    assert_eq!(rejected.len(), 1);
    assert_eq!(rejected[0].der, junk);
    assert_ne!(rejected[0].reason, "");
    let _ = format!("{:?}", TlsClient::new([]).unwrap());
}

#[tokio::test]
async fn a_root_the_verifier_can_use_is_not_listed_and_a_bad_one_beside_it_does_not_spoil_it() {
    let server = server("host.test", (-1, 1)).await;
    let junk = CertificateDer::from(vec![9_u8; 12]);
    let client = TlsClient::new([junk, server.root.clone()]).unwrap();
    assert_eq!(client.rejected_roots().len(), 1);
    assert_eq!(dial(&server, &client, "host.test").await.unwrap(), "hello");
    assert_eq!(
        TlsClient::new([server.root.clone()])
            .unwrap()
            .rejected_roots(),
        []
    );
}
