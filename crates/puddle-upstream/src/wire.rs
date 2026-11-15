// SPDX-License-Identifier: AGPL-3.0-or-later
//! The small piece of HTTP/1.1 a client of a proxy needs: write a request head, read a response
//! head with hard limits. Everything read here comes from a proxy that may be broken or hostile,
//! so nothing is trusted: heads are bounded, lengths are capped, parsing never panics.

use std::io;

use tokio::io::{AsyncRead, AsyncReadExt};

/// Largest response head accepted.
pub(crate) const MAX_RESPONSE_HEAD: usize = 16 * 1024;

/// Largest body of a `407` that is read and thrown away to keep the connection.
pub(crate) const MAX_DRAIN: usize = 64 * 1024;

/// A parsed response head.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResponseHead {
    pub(crate) status: u16,
    pub(crate) reason: String,
    pub(crate) http10: bool,
    /// Header names lower-cased; values trimmed.
    pub(crate) headers: Vec<(String, String)>,
}

impl ResponseHead {
    /// Every value of `name` (a lower-case header name), in order.
    pub(crate) fn values<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a str> {
        self.headers
            .iter()
            .filter(move |(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    /// `Content-Length`, when there is exactly one valid value.
    pub(crate) fn content_length(&self) -> Option<usize> {
        let mut values = self.values("content-length");
        let first = values.next()?;
        if values.next().is_some() {
            return None;
        }
        first.parse().ok()
    }

    /// Whether the connection can carry another request: HTTP/1.1 without `close`.
    pub(crate) fn keeps_alive(&self) -> bool {
        if self.http10 {
            return false;
        }
        !self
            .values("connection")
            .chain(self.values("proxy-connection"))
            .any(|v| v.split(',').any(|t| t.trim().eq_ignore_ascii_case("close")))
    }

    /// The challenges of every `Proxy-Authenticate` header, in order.
    pub(crate) fn challenges(&self) -> Vec<&str> {
        self.values("proxy-authenticate").collect()
    }
}

/// The scheme words of `challenges` (`Negotiate`, `NTLM`, `Basic`), de-duplicated, in order.
/// A value may carry several challenges separated by commas; only the leading word of each
/// header is a scheme name here, which is how real proxies send them (one scheme per header).
pub(crate) fn schemes(challenges: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for challenge in challenges {
        if let Some(word) = challenge.split_whitespace().next() {
            let word = word.trim_end_matches(',');
            if !word.is_empty() && !out.iter().any(|s| s.eq_ignore_ascii_case(word)) {
                out.push(word.to_owned());
            }
        }
    }
    out
}

/// Reads a response head one byte at a time, so that nothing after it is consumed: a tunnel's
/// first bytes belong to the client.
pub(crate) async fn read_response_head<R: AsyncRead + Unpin>(
    r: &mut R,
) -> io::Result<ResponseHead> {
    let mut buf = Vec::with_capacity(256);
    loop {
        if buf.len() >= MAX_RESPONSE_HEAD {
            return Err(bad("response head too large"));
        }
        buf.push(r.read_u8().await.map_err(|err| {
            if err.kind() == io::ErrorKind::UnexpectedEof {
                io::Error::new(io::ErrorKind::UnexpectedEof, "proxy closed the connection")
            } else {
                err
            }
        })?);
        if buf.ends_with(b"\r\n\r\n") || buf.ends_with(b"\n\n") {
            break;
        }
    }
    parse_head(&buf)
}

fn bad(why: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, why.to_owned())
}

fn parse_head(buf: &[u8]) -> io::Result<ResponseHead> {
    let text = std::str::from_utf8(buf).map_err(|_| bad("non-UTF-8 response head"))?;
    let mut lines = text.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l));
    let status_line = lines.next().ok_or_else(|| bad("empty response"))?;
    let mut parts = status_line.splitn(3, ' ');
    let version = parts.next().unwrap_or_default();
    let http10 = match version {
        "HTTP/1.1" => false,
        "HTTP/1.0" => true,
        _ => return Err(bad("not an HTTP/1.x response")),
    };
    let status: u16 = parts
        .next()
        .and_then(|s| s.parse().ok())
        .filter(|s| (100..=999).contains(s))
        .ok_or_else(|| bad("bad status code"))?;
    let reason = parts.next().unwrap_or_default().trim().to_owned();
    let mut headers = Vec::new();
    for line in lines {
        if line.is_empty() {
            break;
        }
        let (name, value) = line.split_once(':').ok_or_else(|| bad("bad header line"))?;
        headers.push((name.trim().to_ascii_lowercase(), value.trim().to_owned()));
    }
    Ok(ResponseHead {
        status,
        reason,
        http10,
        headers,
    })
}

#[cfg(test)]
mod tests;
