// SPDX-License-Identifier: GPL-3.0-or-later
//! The guest's side of a terminated connection: a TLS server whose one certificate is the
//! workspace CA's leaf for the host the guest `CONNECT`ed to.

use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::task::{Context, Poll};

use puddle_ca::WorkspaceCa;
use puddle_netpolicy::normalise_host;
use puddle_types::Host;
use rustls::ServerConfig;
use rustls::crypto::CryptoProvider;
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

/// The protocol names of ALPN (RFC 7301) Puddle speaks on a terminated connection.
pub(crate) const ALPN_HTTP11: &[u8] = b"http/1.1";
pub(crate) const ALPN_H2: &[u8] = b"h2";

/// What the guest's `ClientHello` says, which decides what the upstream is offered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Hello {
    /// The name asked for is the `CONNECT` host.
    pub(crate) name_matches: bool,
    /// ALPN lists `h2`.
    pub(crate) offers_h2: bool,
    /// ALPN lists `http/1.1`, or there is no ALPN at all (such a client speaks HTTP/1.1).
    pub(crate) offers_h1: bool,
}

/// Which protocol a leg of a terminated connection speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Proto {
    H1,
    H2,
}

impl Proto {
    pub(crate) fn alpn(self) -> &'static [u8] {
        match self {
            Self::H1 => ALPN_HTTP11,
            Self::H2 => ALPN_H2,
        }
    }

    pub(crate) fn from_alpn(chosen: Option<&[u8]>) -> Self {
        if chosen == Some(ALPN_H2) {
            Self::H2
        } else {
            Self::H1
        }
    }
}

impl Hello {
    /// The protocols to offer the real server, best first: `h2` whenever the guest can speak it
    /// (a server that only speaks HTTP/1.1 then answers `http/1.1`, and the proxy translates),
    /// else `http/1.1`.
    pub(crate) fn upstream_offer(self) -> Vec<&'static [u8]> {
        if self.offers_h2 {
            vec![ALPN_H2, ALPN_HTTP11]
        } else {
            vec![ALPN_HTTP11]
        }
    }

    /// The protocol the guest is served, given what the real server chose: the same one, except
    /// that a guest that offered only `h2` gets `h2` over an HTTP/1.1 server (translated), and a
    /// guest that offered both gets the server's own choice.
    pub(crate) fn guest_proto(self, upstream: Proto) -> Proto {
        match upstream {
            Proto::H2 => Proto::H2,
            Proto::H1 if self.offers_h2 && !self.offers_h1 => Proto::H2,
            Proto::H1 => Proto::H1,
        }
    }
}

/// Reads the guest's `ClientHello` without answering it, so that the proxy can look at the
/// name and the protocols before it decides anything.
///
/// # Errors
/// The stream failed, or what arrived is not a `ClientHello`.
pub(crate) async fn read_hello<S>(
    stream: S,
    host: &Host,
) -> io::Result<(tokio_rustls::StartHandshake<S>, Hello)>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let start =
        tokio_rustls::LazyConfigAcceptor::new(rustls::server::Acceptor::default(), stream).await?;
    let hello = hello_of(&start.client_hello(), host);
    Ok((start, hello))
}

fn hello_of(hello: &ClientHello<'_>, host: &Host) -> Hello {
    let name_matches = name_is(hello.server_name(), &host.to_string());
    let (mut offers_h2, mut offers_h1, mut any) = (false, false, false);
    for proto in hello.alpn().into_iter().flatten() {
        any = true;
        offers_h2 |= proto == ALPN_H2;
        offers_h1 |= proto == ALPN_HTTP11;
    }
    Hello {
        name_matches,
        offers_h2,
        offers_h1: offers_h1 || !any,
    }
}

/// What a guest's connection starts with.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum FirstBytes {
    /// One complete TLS record that begins a handshake, as the bytes were read.
    Tls(Vec<u8>),
    /// Anything else (not TLS, a record cut short, nothing at all within the wait), as read, so
    /// that a pass-through can send it on.
    Other(Vec<u8>),
}

/// The size of a TLS record header: type, version, length.
const RECORD_HEADER: usize = 5;

/// Reads the first TLS record of `stream` after `early` (bytes already read), without
/// answering it: the bytes stay available, so that the connection can still be passed through
/// to the real server untouched when it is not something Puddle can decrypt.
///
/// `first` bounds the wait for the first byte (a protocol where the server speaks first sends
/// nothing, and must not wait for a handshake timeout); `rest` bounds the wait for the others.
///
/// # Errors
/// The stream failed with an error other than ending.
pub(crate) async fn capture_first_record<S>(
    stream: &mut S,
    early: Vec<u8>,
    first: std::time::Duration,
    rest: std::time::Duration,
) -> io::Result<FirstBytes>
where
    S: AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt as _;
    let mut buf = early;
    let mut want = RECORD_HEADER;
    let mut patience = first;
    loop {
        if buf.len() >= want {
            if want == RECORD_HEADER {
                let [kind, major, _, hi, lo, ..] = buf.as_slice() else {
                    return Ok(FirstBytes::Other(buf));
                };
                let (kind, major) = (*kind, *major);
                let len = usize::from(u16::from_be_bytes([*hi, *lo]));
                // Handshake record, SSL 3 or TLS, at most the 16 KiB a record may hold.
                if kind != 0x16 || major != 3 || len == 0 || len > MAX_RECORD {
                    return Ok(FirstBytes::Other(buf));
                }
                want = RECORD_HEADER + len;
                continue;
            }
            // A ClientHello is handshake type 1.
            return Ok(if buf.get(RECORD_HEADER) == Some(&1) {
                FirstBytes::Tls(buf)
            } else {
                FirstBytes::Other(buf)
            });
        }
        let mut chunk = [0u8; 4096];
        match tokio::time::timeout(patience, stream.read(&mut chunk)).await {
            Ok(Ok(0)) | Err(_) => return Ok(FirstBytes::Other(buf)),
            Ok(Ok(n)) => buf.extend_from_slice(chunk.get(..n).unwrap_or_default()),
            Ok(Err(err)) => return Err(err),
        }
        patience = rest;
    }
}

/// The most a TLS record holds (RFC 8446 §5.1).
const MAX_RECORD: usize = 16 * 1024;

/// What the `ClientHello` in `captured` says, read without a connection. `None` when `captured`
/// is not a whole `ClientHello` (a hello that spans records, which no common client sends).
pub(crate) fn parse_hello(captured: &[u8], host: &Host) -> Option<Hello> {
    let mut acceptor = rustls::server::Acceptor::default();
    let mut rest = captured;
    while !rest.is_empty() {
        acceptor.read_tls(&mut rest).ok()?;
        if let Some(accepted) = acceptor.accept().ok()? {
            return Some(hello_of(&accepted.client_hello(), host));
        }
    }
    None
}

/// Whether a `ClientHello`'s name is `host`.
fn name_is(sni: Option<&str>, host: &str) -> bool {
    let wanted = sni
        .and_then(|name| normalise_host(name).ok())
        .map(puddle_netpolicy::Target::into_host);
    matches!(&wanted, Some(Host::Name(name)) if name.to_string() == host)
}

fn provider() -> Arc<CryptoProvider> {
    static PROVIDER: OnceLock<Arc<CryptoProvider>> = OnceLock::new();
    Arc::clone(PROVIDER.get_or_init(|| Arc::new(rustls::crypto::ring::default_provider())))
}

/// Serves the leaf for `host` to a client that asks for `host` by name, and for nothing else.
///
/// The check is on the name in the `ClientHello`: a client that sends no name, an IP address or
/// another name gets a TLS alert and no certificate, and the connection never reaches the
/// upstream. Only the one leaf is ever reachable through it, whatever the client asks.
#[derive(Debug)]
struct SniGate {
    ca: Arc<WorkspaceCa>,
    host: String,
    flags: Arc<GateFlags>,
}

/// What the gate saw go wrong on one connection.
#[derive(Debug, Default)]
pub(crate) struct GateFlags {
    /// The client asked for another name, or none.
    pub(crate) mismatch: AtomicBool,
    /// The client asked for the right name and the CA could not make a certificate for it.
    pub(crate) leaf_failed: AtomicBool,
}

impl ResolvesServerCert for SniGate {
    fn resolve(&self, hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        if !name_is(hello.server_name(), &self.host) {
            self.flags.mismatch.store(true, Ordering::Relaxed);
            return None;
        }
        match self.ca.leaf(&self.host) {
            Ok(leaf) => Some(leaf),
            Err(err) => {
                tracing::warn!(host = %self.host, error = %err, "no certificate for a bound host");
                self.flags.leaf_failed.store(true, Ordering::Relaxed);
                None
            }
        }
    }
}

/// The server configuration for one connection to `host`, offering `alpn`, and the flags that say
/// what the gate refused.
pub(crate) fn server_config(
    ca: &Arc<WorkspaceCa>,
    host: &Host,
    alpn: &[&[u8]],
) -> Result<(Arc<ServerConfig>, Arc<GateFlags>), rustls::Error> {
    let flags = Arc::new(GateFlags::default());
    let gate = SniGate {
        ca: Arc::clone(ca),
        host: host.to_string(),
        flags: Arc::clone(&flags),
    };
    let mut config = ServerConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()?
        .with_no_client_auth()
        .with_cert_resolver(Arc::new(gate));
    config.alpn_protocols = alpn.iter().map(|p| p.to_vec()).collect();
    // One connection per config: nothing to resume, and no tickets that outlive it.
    config.send_tls13_tickets = 0;
    config.session_storage = Arc::new(rustls::server::NoServerSessionStorage {});
    Ok((Arc::new(config), flags))
}

/// A stream that first yields bytes already read from it, then reads on.
#[derive(Debug)]
pub(crate) struct Prefixed<S> {
    prefix: io::Cursor<Vec<u8>>,
    inner: S,
}

impl<S> Prefixed<S> {
    pub(crate) fn new(inner: S, prefix: Vec<u8>) -> Self {
        Self {
            prefix: io::Cursor::new(prefix),
            inner,
        }
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for Prefixed<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = &mut *self;
        let start = usize::try_from(this.prefix.position()).unwrap_or(usize::MAX);
        let rest = this.prefix.get_ref().get(start..).unwrap_or_default();
        if !rest.is_empty() {
            let n = rest.len().min(buf.remaining());
            buf.put_slice(rest.get(..n).unwrap_or_default());
            this.prefix.set_position((start + n) as u64);
            return Poll::Ready(Ok(()));
        }
        Pin::new(&mut this.inner).poll_read(cx, buf)
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Prefixed<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::io::AsyncReadExt;

    use super::*;

    /// A real `ClientHello` for `name` with `alpn`, as a TLS client sends it.
    fn client_hello(name: &str, alpn: &[&[u8]]) -> Vec<u8> {
        let mut roots = rustls::RootCertStore::empty();
        roots.add_parsable_certificates(Vec::new());
        let mut config = rustls::ClientConfig::builder_with_provider(provider())
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth();
        config.alpn_protocols = alpn.iter().map(|p| p.to_vec()).collect();
        let mut conn = rustls::ClientConnection::new(
            Arc::new(config),
            rustls::pki_types::ServerName::try_from(name.to_owned()).unwrap(),
        )
        .unwrap();
        let mut out = Vec::new();
        conn.write_tls(&mut out).unwrap();
        out
    }

    async fn capture(bytes: Vec<u8>, early: &[u8]) -> FirstBytes {
        let (mut a, b) = tokio::io::duplex(64 * 1024);
        tokio::io::AsyncWriteExt::write_all(&mut a, &bytes)
            .await
            .unwrap();
        let mut reader = b;
        let wait = Duration::from_millis(200);
        let result = capture_first_record(&mut reader, early.to_vec(), wait, wait).await;
        drop(a);
        result.unwrap()
    }

    #[tokio::test]
    async fn a_client_hello_is_captured_whole_and_read_without_a_connection() {
        let hello = client_hello("free.test", &[b"h2", b"http/1.1"]);
        assert_eq!(
            capture(hello.clone(), &[]).await,
            FirstBytes::Tls(hello.clone())
        );
        let host = Host::parse_normalised("free.test").unwrap();
        let read = parse_hello(&hello, &host).unwrap();
        assert!(read.name_matches && read.offers_h2 && read.offers_h1);
        let other = Host::parse_normalised("other.test").unwrap();
        assert!(!parse_hello(&hello, &other).unwrap().name_matches);
        let h2_only = client_hello("free.test", &[b"h2"]);
        let read = parse_hello(&h2_only, &host).unwrap();
        assert!(read.offers_h2 && !read.offers_h1);
    }

    #[tokio::test]
    async fn bytes_read_before_the_capture_count_as_the_start_of_the_record() {
        let hello = client_hello("free.test", &[]);
        let (early, rest) = hello.split_at(3);
        assert_eq!(
            capture(rest.to_vec(), early).await,
            FirstBytes::Tls(hello.clone())
        );
        assert_eq!(capture(Vec::new(), &hello).await, FirstBytes::Tls(hello));
    }

    #[tokio::test]
    async fn anything_that_is_not_one_whole_client_hello_is_other_and_keeps_its_bytes() {
        let hello = client_hello("free.test", &[]);
        let cases: Vec<Vec<u8>> = vec![
            b"GET / HTTP/1.1

"
            .to_vec(),
            b"SSH".to_vec(),
            Vec::new(),
            // A handshake record that is not a ClientHello.
            vec![0x16, 3, 3, 0, 4, 2, 0, 0, 0],
            // Application data, a zero-length record, a record over 16 KiB.
            vec![0x17, 3, 3, 0, 1, 0],
            vec![0x16, 3, 3, 0, 0],
            vec![0x16, 3, 3, 0x7f, 0xff],
            // A record cut short.
            hello[..hello.len() - 1].to_vec(),
        ];
        for bytes in cases {
            assert_eq!(capture(bytes.clone(), &[]).await, FirstBytes::Other(bytes));
        }
    }

    #[test]
    fn bytes_that_are_not_a_whole_client_hello_give_no_hello() {
        let host = Host::parse_normalised("free.test").unwrap();
        let hello = client_hello("free.test", &[]);
        assert!(parse_hello(&hello[..hello.len() - 1], &host).is_none());
        assert!(parse_hello(b"GET / HTTP/1.1\r\n\r\n", &host).is_none());
        assert!(parse_hello(&[], &host).is_none());
    }

    #[tokio::test]
    async fn the_prefix_is_read_before_the_stream() {
        let (mut a, b) = tokio::io::duplex(64);
        tokio::io::AsyncWriteExt::write_all(&mut a, b"-tail")
            .await
            .unwrap();
        drop(a);
        let mut s = Prefixed::new(b, b"head".to_vec());
        let mut out = String::new();
        s.read_to_string(&mut out).await.unwrap();
        assert_eq!(out, "head-tail");
    }
}
