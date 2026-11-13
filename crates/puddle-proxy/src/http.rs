// SPDX-License-Identifier: AGPL-3.0-or-later
//! Just enough HTTP/1.1 for a forward proxy: the request head with hard limits, the request
//! target, and request-body framing, so a request ends exactly where its framing says and the
//! bytes after it are read as the next request, never as part of this one.
//!
//! Everything here parses guest input, so it never trusts a length, never buffers more than
//! [`MAX_HEAD`], and refuses ambiguous framing instead of guessing.

use std::fmt::Write as _;
use std::io;

use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Largest request head (request line plus headers). Larger heads get `431`, so a guest can't
/// grow the proxy's memory.
pub(crate) const MAX_HEAD: usize = 64 * 1024;

/// Most header lines in one head.
pub(crate) const MAX_HEADERS: usize = 200;

/// Longest chunk-size line in a chunked body.
pub(crate) const MAX_CHUNK_LINE: usize = 1024;

/// Longest trailer line in a chunked body.
pub(crate) const MAX_TRAILER_LINE: usize = 8 * 1024;

/// A request head as received.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Head {
    /// `CONNECT`, `GET`, ... as sent (case kept).
    pub(crate) method: String,
    /// The request target as sent.
    pub(crate) uri: String,
    /// `HTTP/1.1` or `HTTP/1.0`.
    pub(crate) version: String,
    /// Raw header lines, line ending removed.
    pub(crate) headers: Vec<String>,
}

impl Head {
    /// Whether this is a `CONNECT` request.
    pub(crate) fn is_connect(&self) -> bool {
        self.method.eq_ignore_ascii_case("CONNECT")
    }
}

/// Why a head was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HeadError {
    /// The connection closed before a request line.
    Empty,
    /// Over [`MAX_HEAD`] bytes or [`MAX_HEADERS`] lines: `431`.
    TooLarge,
    /// Malformed: `400`, with the reason.
    Bad(&'static str),
}

/// Reads the request head. Never buffers more than [`MAX_HEAD`] bytes, whatever the guest sends.
///
/// The outer `Err` is an I/O error on the stream; the inner one a head the proxy refuses.
pub(crate) async fn read_head<R: AsyncBufRead + Unpin>(
    r: &mut R,
) -> io::Result<Result<Head, HeadError>> {
    let mut budget = MAX_HEAD;
    let mut lines: Vec<String> = Vec::new();
    loop {
        let mut buf = Vec::new();
        let n = (&mut *r)
            .take(budget as u64 + 1)
            .read_until(b'\n', &mut buf)
            .await?;
        if n == 0 {
            return Ok(Err(if lines.is_empty() {
                HeadError::Empty
            } else {
                HeadError::Bad("head ended early")
            }));
        }
        if n > budget {
            return Ok(Err(HeadError::TooLarge));
        }
        budget -= n;
        if !buf.ends_with(b"\n") {
            return Ok(Err(HeadError::Bad("head ended early")));
        }
        let Ok(line) = String::from_utf8(buf) else {
            return Ok(Err(HeadError::Bad("non-UTF-8 head")));
        };
        let line = line
            .strip_suffix('\n')
            .map_or(line.as_str(), |l| l.strip_suffix('\r').unwrap_or(l));
        if line.bytes().any(|b| b == 0x7f || (b < 0x20 && b != b'\t')) {
            return Ok(Err(HeadError::Bad("control character in head")));
        }
        if line.is_empty() {
            if lines.is_empty() {
                continue; // RFC 9112 §2.2: ignore an empty line before the request line
            }
            break;
        }
        lines.push(line.to_owned());
        if lines.len() > MAX_HEADERS + 1 {
            return Ok(Err(HeadError::TooLarge));
        }
    }
    let mut lines = lines.into_iter();
    let request_line = lines.next().unwrap_or_default();
    let headers: Vec<String> = lines.collect();
    let mut parts = request_line.split(' ');
    let (Some(method), Some(uri), Some(version), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Ok(Err(HeadError::Bad("malformed request line")));
    };
    if method.is_empty() || uri.is_empty() || !method.bytes().all(is_tchar) {
        return Ok(Err(HeadError::Bad("malformed request line")));
    }
    if version != "HTTP/1.1" && version != "HTTP/1.0" {
        return Ok(Err(HeadError::Bad("unsupported HTTP version")));
    }
    if headers.iter().any(|h| !valid_header_line(h)) {
        return Ok(Err(HeadError::Bad("malformed header line")));
    }
    Ok(Ok(Head {
        method: method.to_owned(),
        uri: uri.to_owned(),
        version: version.to_owned(),
        headers,
    }))
}

/// RFC 9110 `tchar`: what a method or header name may contain.
pub(crate) fn is_tchar(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b)
}

/// A `name: value` line with a token name and no obsolete line folding.
fn valid_header_line(line: &str) -> bool {
    match line.split_once(':') {
        Some((name, _)) => !name.is_empty() && name.bytes().all(is_tchar),
        None => false,
    }
}

/// The lower-cased name of a header line.
pub(crate) fn header_name(line: &str) -> String {
    line.split_once(':')
        .map_or(line, |(name, _)| name)
        .to_ascii_lowercase()
}

/// The trimmed value of a header line.
pub(crate) fn header_value(line: &str) -> &str {
    line.split_once(':').map_or("", |(_, v)| v.trim())
}

/// The destination a request names, before host normalisation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RawTarget {
    /// The host as sent, without brackets around an IPv6 literal.
    pub(crate) host: String,
    /// The port: from the request, or 80 for an absolute `http://` URI without one.
    pub(crate) port: u16,
    /// For an absolute-form request: the origin-form path (`/a?b`) to send upstream.
    pub(crate) path: Option<String>,
}

/// `CONNECT host:port`, or an absolute-form `GET http://host[:port]/path`. Anything else is a
/// client that wasn't pointed at a proxy.
pub(crate) fn parse_target(method: &str, uri: &str) -> Result<RawTarget, &'static str> {
    if method.eq_ignore_ascii_case("CONNECT") {
        let (host, port) = split_host_port(uri)?;
        let port = port.ok_or("CONNECT needs host:port")?;
        return Ok(RawTarget {
            host,
            port,
            path: None,
        });
    }
    let rest = match uri.get(..7) {
        Some(scheme) if scheme.eq_ignore_ascii_case("http://") => uri.get(7..).unwrap_or(""),
        _ => return Err("not a proxy request (expected CONNECT or an absolute http:// URI)"),
    };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(end);
    let tail = tail.split('#').next().unwrap_or("");
    let path = if tail.starts_with('/') {
        tail.to_owned()
    } else {
        format!("/{tail}")
    };
    let (host, port) = split_host_port(authority)?;
    Ok(RawTarget {
        host,
        port: port.unwrap_or(80),
        path: Some(path),
    })
}

/// `host`, `host:port`, `[v6]` or `[v6]:port`. Userinfo is refused, not stripped: it has no
/// business in a proxy request and hides the real host from anyone reading the line.
pub(crate) fn split_host_port(authority: &str) -> Result<(String, Option<u16>), &'static str> {
    if authority.contains('@') {
        return Err("userinfo (user@host) is not allowed");
    }
    let (host, port) = if let Some(rest) = authority.strip_prefix('[') {
        let (h, after) = rest.split_once(']').ok_or("unclosed [ in host")?;
        if !h.contains(':') {
            return Err("brackets around a non-IPv6 host");
        }
        match after {
            "" => (h, None),
            p => (h, Some(p.strip_prefix(':').ok_or("junk after ]")?)),
        }
    } else {
        match authority.split_once(':') {
            Some((h, p)) => (h, Some(p)),
            None => (authority, None),
        }
    };
    if host.is_empty() {
        return Err("empty host");
    }
    let port = match port {
        None | Some("") => None, // RFC 3986: an empty port means the default
        Some(p) if p.bytes().all(|b| b.is_ascii_digit()) => match p.parse::<u16>() {
            Ok(0) | Err(_) => return Err("port out of range"),
            Ok(n) => Some(n),
        },
        Some(_) => return Err("port is not a number"),
    };
    Ok((host.to_owned(), port))
}

/// How a request body is framed (RFC 9112 §6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Body {
    /// No body.
    None,
    /// Exactly this many bytes.
    Length(u64),
    /// Chunked transfer coding.
    Chunked,
}

/// Request-body framing. Ambiguous framing is refused: a proxy and a server that disagree on where
/// a request ends is how a second, unchecked request gets smuggled through.
pub(crate) fn body_framing(headers: &[String]) -> Result<Body, &'static str> {
    let mut length: Option<u64> = None;
    let mut codings: Vec<String> = Vec::new();
    for h in headers {
        match header_name(h).as_str() {
            "content-length" => {
                for v in header_value(h).split(',') {
                    let v = v.trim();
                    if v.is_empty() || !v.bytes().all(|b| b.is_ascii_digit()) {
                        return Err("bad Content-Length");
                    }
                    let n: u64 = v.parse().map_err(|_| "bad Content-Length")?;
                    if length.is_some_and(|l| l != n) {
                        return Err("conflicting Content-Length");
                    }
                    length = Some(n);
                }
            }
            "transfer-encoding" => codings.extend(
                header_value(h)
                    .split(',')
                    .map(|v| v.trim().to_ascii_lowercase())
                    .filter(|v| !v.is_empty()),
            ),
            _ => {}
        }
    }
    if codings.is_empty() {
        return Ok(length.map_or(Body::None, Body::Length));
    }
    if length.is_some() {
        return Err("both Content-Length and Transfer-Encoding");
    }
    if codings.last().map(String::as_str) != Some("chunked") {
        return Err("Transfer-Encoding without final chunked");
    }
    Ok(Body::Chunked)
}

/// Copies exactly one request body from `r` to `w`, so bytes after it (a pipelined second
/// request) are never forwarded.
pub(crate) async fn copy_body<R, W>(r: &mut R, w: &mut W, body: Body) -> io::Result<()>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    match body {
        Body::None => Ok(()),
        Body::Length(n) => copy_exact(r, w, n, "body shorter than Content-Length").await,
        Body::Chunked => loop {
            let line = read_line_limited(r, MAX_CHUNK_LINE).await?;
            w.write_all(&line).await?;
            let size = chunk_size(&line)?;
            if size == 0 {
                // Trailer section, up to the empty line.
                loop {
                    let t = read_line_limited(r, MAX_TRAILER_LINE).await?;
                    w.write_all(&t).await?;
                    if t == b"\r\n" || t == b"\n" {
                        return Ok(());
                    }
                }
            }
            copy_exact(r, w, size, "chunk cut short").await?;
            let crlf = read_line_limited(r, 2).await?;
            if crlf != b"\r\n" && crlf != b"\n" {
                return Err(invalid("chunk not followed by CRLF"));
            }
            w.write_all(&crlf).await?;
        },
    }
}

async fn copy_exact<R, W>(r: &mut R, w: &mut W, n: u64, short: &'static str) -> io::Result<()>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let copied = tokio::io::copy(&mut (&mut *r).take(n), w).await?;
    if copied == n {
        Ok(())
    } else {
        Err(io::Error::new(io::ErrorKind::UnexpectedEof, short))
    }
}

/// The size in a chunk-size line (`1a;ext=1\r\n`).
pub(crate) fn chunk_size(line: &[u8]) -> io::Result<u64> {
    let text = std::str::from_utf8(line).map_err(|_| invalid("bad chunk size"))?;
    let hex = text.trim_end().split(';').next().unwrap_or("").trim();
    if hex.is_empty() || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(invalid("bad chunk size"));
    }
    u64::from_str_radix(hex, 16).map_err(|_| invalid("bad chunk size"))
}

pub(crate) fn invalid(what: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what)
}

pub(crate) async fn read_line_limited<R: AsyncBufRead + Unpin>(
    r: &mut R,
    limit: usize,
) -> io::Result<Vec<u8>> {
    let mut buf = Vec::new();
    (&mut *r)
        .take(limit as u64)
        .read_until(b'\n', &mut buf)
        .await?;
    if buf.ends_with(b"\n") {
        Ok(buf)
    } else {
        Err(invalid("line too long or cut short"))
    }
}

/// Request headers a proxy never forwards (RFC 9110 §7.6.1), plus `Host` (replaced) and the
/// proxy's own credentials.
const HOP_BY_HOP: [&str; 8] = [
    "connection",
    "keep-alive",
    "proxy-connection",
    "proxy-authorization",
    "proxy-authenticate",
    "host",
    "te",
    "upgrade",
];

/// The prefix of the header names Puddle uses between the guest agent and the proxy, and on its
/// own answers. None of them travels upstream, whoever wrote it.
const PUDDLE_HEADER_PREFIX: &str = "x-puddle-";

/// The head sent upstream for a plain-HTTP request. Hop-by-hop and proxy headers and Puddle's
/// own header names are dropped, `Host` is replaced by the checked target (RFC 9112 §3.2.2: a proxy ignores the client's `Host`
/// for an absolute-form target), and `Connection: close` ends the upstream connection after this
/// one request, so nothing on it goes unchecked.
pub(crate) fn upstream_head(
    head: &Head,
    target: &str,
    host_header: &str,
    proxy_authorization: Option<&str>,
) -> String {
    let mut hop: Vec<String> = HOP_BY_HOP.iter().map(|h| (*h).to_owned()).collect();
    for h in &head.headers {
        if header_name(h) == "connection" {
            hop.extend(
                header_value(h)
                    .split(',')
                    .map(|v| v.trim().to_ascii_lowercase()),
            );
        }
    }
    let mut out = format!("{} {target} {}\r\n", head.method, head.version);
    let _ = write!(out, "Host: {host_header}\r\n");
    if let Some(value) = proxy_authorization {
        let _ = write!(out, "Proxy-Authorization: {value}\r\n");
    }
    for h in &head.headers {
        let name = header_name(h);
        if !hop.contains(&name) && !name.starts_with(PUDDLE_HEADER_PREFIX) {
            out.push_str(h);
            out.push_str("\r\n");
        }
    }
    out.push_str("Connection: close\r\n\r\n");
    out
}

#[cfg(test)]
mod tests;
