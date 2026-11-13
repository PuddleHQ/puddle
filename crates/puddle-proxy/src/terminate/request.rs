// SPDX-License-Identifier: AGPL-3.0-or-later
//! One request read from a terminated connection: checked, then rebuilt for the upstream.
//!
//! The checks are the plain-HTTP path's (`http.rs`: size limits, no obsolete line folding, no
//! `Content-Length` with `Transfer-Encoding`, no invalid coding) plus what only a terminated
//! connection needs: the request must be for the host the certificate and the `CONNECT` named.
//! A request is never forwarded as received. It is rebuilt from the parsed pieces, so what the
//! upstream sees is what was checked.

use ::http::header::{HeaderMap, HeaderName, HeaderValue};
use puddle_netpolicy::normalise_host;

use super::inject::Injection;
use crate::http::{self, Body, Head};
use crate::proxy::Refusal;
use crate::target::Target;

/// The default port of `https`, the only port that is terminated.
pub(crate) const HTTPS_PORT: u16 = 443;

/// Headers the proxy sets or owns itself: never copied from the guest, never set by an injector.
pub(crate) fn is_proxy_owned(name: &HeaderName) -> bool {
    matches!(
        name.as_str(),
        "host"
            | "connection"
            | "keep-alive"
            | "proxy-connection"
            | "proxy-authorization"
            | "proxy-authenticate"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
            | "content-length"
            | "expect"
    )
}

/// A request that passed the checks.
#[derive(Debug)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent facts about one request head; an enum would only rename them"
)]
pub(crate) struct Parsed {
    pub(crate) method: String,
    /// Origin form: path and query, starting with `/`.
    pub(crate) target: String,
    pub(crate) http11: bool,
    pub(crate) body: Body,
    /// The guest waits for `100 Continue` before sending the body.
    pub(crate) expect_continue: bool,
    /// The guest asked to close after this request.
    pub(crate) close: bool,
    /// A WebSocket handshake (`Connection: upgrade`, `Upgrade: websocket`).
    pub(crate) upgrade: bool,
}

pub(crate) fn bad(why: impl Into<String>) -> Refusal {
    Refusal::new("400 Bad Request", why).coded("bad_request")
}

/// Checks `head` as a request to `target` (the `CONNECT` host, port 443).
pub(crate) fn parse(head: &Head, target: &Target) -> Result<Parsed, Refusal> {
    if head.is_connect() {
        return Err(bad("CONNECT inside a tunnel is not supported"));
    }
    let http11 = head.version == "HTTP/1.1";
    let path = origin_target(&head.uri, target)?;
    let hosts: Vec<&str> = head
        .headers
        .iter()
        .filter(|h| http::header_name(h) == "host")
        .map(|h| http::header_value(h))
        .collect();
    match hosts.as_slice() {
        [] if http11 => return Err(bad("missing Host header")),
        [] => {}
        [host] => {
            if !authority_is(host, target) {
                return Err(misdirected(target));
            }
        }
        _ => return Err(bad("more than one Host header")),
    }
    framed(head, path)
}

/// The checks every HTTP/1.x request gets, wherever it came from, with `path` as its origin-form
/// target: unambiguous body framing, no chunked body from HTTP/1.0, `Expect` only for
/// `100-continue`, and what the head says about the connection.
pub(crate) fn framed(head: &Head, path: String) -> Result<Parsed, Refusal> {
    // Before any rule is asked or any upstream connection made: a target the upstream request
    // cannot carry must not reach either.
    path.parse::<::http::uri::PathAndQuery>()
        .map_err(|_| bad("malformed request target"))?;
    let http11 = head.version == "HTTP/1.1";
    let body = http::body_framing(&head.headers).map_err(bad)?;
    if !http11 && body == Body::Chunked {
        return Err(bad("Transfer-Encoding in an HTTP/1.0 request"));
    }
    let mut expect_continue = false;
    for line in head
        .headers
        .iter()
        .filter(|h| http::header_name(h) == "expect")
    {
        if !http::header_value(line).eq_ignore_ascii_case("100-continue") {
            return Err(Refusal::new(
                "417 Expectation Failed",
                "only Expect: 100-continue is supported",
            )
            .coded("expectation_unsupported"));
        }
        expect_continue = http11 && body != Body::None;
    }
    let tokens = connection_tokens(head);
    let close = !http11 || tokens.iter().any(|t| t == "close");
    let upgrade_values: Vec<&str> = head
        .headers
        .iter()
        .filter(|h| http::header_name(h) == "upgrade")
        .map(|h| http::header_value(h))
        .collect();
    let upgrade = http11
        && head.method == "GET"
        && body == Body::None
        && super::ws::is_upgrade_request(&tokens, &upgrade_values);
    Ok(Parsed {
        method: head.method.clone(),
        target: path,
        http11,
        body,
        expect_continue,
        close,
        upgrade,
    })
}

pub(crate) fn misdirected(target: &Target) -> Refusal {
    Refusal::new(
        "421 Misdirected Request",
        format!(
            "this connection is for {}; send requests for other hosts on their own connection",
            target.host
        ),
    )
    .coded("misdirected_host")
}

/// Whether an authority (`host` or `host:port`) names `target`.
pub(crate) fn authority_is(authority: &str, target: &Target) -> bool {
    let Ok((host, port)) = http::split_host_port(authority) else {
        return false;
    };
    let Ok(host) = normalise_host(&host) else {
        return false;
    };
    host.into_host() == target.host && port.unwrap_or(HTTPS_PORT) == target.port
}

/// The path and query of the request target, which is origin-form or absolute-form for this
/// connection's own host.
fn origin_target(uri: &str, target: &Target) -> Result<String, Refusal> {
    if uri.contains('#') {
        return Err(bad("a fragment in the request target"));
    }
    let path = if uri.starts_with('/') {
        uri.to_owned()
    } else if let Some((scheme, rest)) = uri.split_once("://") {
        if !scheme.eq_ignore_ascii_case("https") {
            return Err(bad("an absolute http:// target inside an https connection"));
        }
        let end = rest.find(['/', '?']).unwrap_or(rest.len());
        let (authority, tail) = rest.split_at(end);
        if authority.contains('@') {
            return Err(bad("userinfo (user@host) is not allowed"));
        }
        if !authority_is(authority, target) {
            return Err(misdirected(target));
        }
        if tail.starts_with('/') {
            tail.to_owned()
        } else {
            format!("/{tail}")
        }
    } else {
        return Err(bad("the request target must be a path"));
    };
    path.parse::<::http::uri::PathAndQuery>()
        .map_err(|_| bad("malformed request target"))?;
    Ok(path)
}

/// The lower-cased tokens of every `Connection` header.
fn connection_tokens(head: &Head) -> Vec<String> {
    head.headers
        .iter()
        .filter(|h| http::header_name(h) == "connection")
        .flat_map(|h| http::header_value(h).split(','))
        .map(|t| t.trim().to_ascii_lowercase())
        .filter(|t| !t.is_empty())
        .collect()
}

/// The headers to send upstream: `Host` first, then the guest's end-to-end headers, then the
/// injected ones. Hop-by-hop headers and the headers the proxy owns are dropped (among them
/// `Proxy-Authorization`, which is addressed to this proxy). The guest's `Authorization` is kept:
/// it is the guest's own, and an injected header replaces a guest header of the same name only.
pub(crate) fn upstream_headers(
    head: &Head,
    target: &Target,
    injection: Option<&Injection>,
) -> Result<HeaderMap, Refusal> {
    let mut map = HeaderMap::new();
    let host = if target.port == HTTPS_PORT {
        target.host.to_string()
    } else {
        format!("{}:{}", target.host, target.port)
    };
    map.insert(
        HeaderName::from_static("host"),
        HeaderValue::from_str(&host).map_err(|_| bad("host"))?,
    );
    let named_in_connection = connection_tokens(head);
    for line in &head.headers {
        let name = HeaderName::from_bytes(http::header_name(line).as_bytes())
            .map_err(|_| bad("malformed header name"))?;
        if is_proxy_owned(&name) || named_in_connection.iter().any(|t| t == name.as_str()) {
            continue;
        }
        let value = HeaderValue::from_bytes(http::header_value(line).as_bytes())
            .map_err(|_| bad("malformed header value"))?;
        map.append(name, value);
    }
    if let Some(injection) = injection {
        for header in injection.headers() {
            let Some(value) = header.header_value() else {
                return Err(Refusal::new(
                    "502 Bad Gateway",
                    "an injected header could not be built",
                ));
            };
            map.insert(header.name().clone(), value);
        }
    }
    Ok(map)
}

/// Which HTTP version the real server is spoken to in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UpstreamVersion {
    H1,
    H2,
}

/// The headers to send upstream for a request that came in over HTTP/2: the guest's end-to-end
/// headers, then the injected ones. The same rules as [`upstream_headers`]: hop-by-hop headers
/// and the headers the proxy owns are dropped, the guest's `Authorization` is kept.
/// `te: trailers` is the one hop-by-hop header kept, and only toward an
/// HTTP/2 server (gRPC needs it). Toward an HTTP/1.1 server `Host` is added and the cookie
/// crumbs are joined into one `Cookie` header (RFC 9113 §8.2.3). The `:authority` of an HTTP/2
/// server comes from the request's URI, so no `Host` is added there.
pub(crate) fn upstream_headers_h2(
    headers: &HeaderMap,
    target: &Target,
    injection: Option<&Injection>,
    version: UpstreamVersion,
) -> Result<HeaderMap, Refusal> {
    let mut map = HeaderMap::new();
    if version == UpstreamVersion::H1 {
        let host = if target.port == HTTPS_PORT {
            target.host.to_string()
        } else {
            format!("{}:{}", target.host, target.port)
        };
        map.insert(
            HeaderName::from_static("host"),
            HeaderValue::from_str(&host).map_err(|_| bad("host"))?,
        );
    }
    let mut cookies: Vec<&[u8]> = Vec::new();
    for (name, value) in headers {
        if name == ::http::header::TE {
            if version == UpstreamVersion::H2 && value.as_bytes().eq_ignore_ascii_case(b"trailers")
            {
                map.insert(name.clone(), value.clone());
            }
            continue;
        }
        if is_proxy_owned(name) {
            continue;
        }
        if version == UpstreamVersion::H1 && name == ::http::header::COOKIE {
            cookies.push(value.as_bytes());
            continue;
        }
        map.append(name.clone(), value.clone());
    }
    if !cookies.is_empty() {
        let joined = cookies.join(&b"; "[..]);
        map.insert(
            ::http::header::COOKIE,
            HeaderValue::from_bytes(&joined).map_err(|_| bad("malformed cookie"))?,
        );
    }
    if let Some(injection) = injection {
        for header in injection.headers() {
            let value = header.header_value().ok_or_else(unbuildable_header)?;
            map.insert(header.name().clone(), value);
        }
    }
    Ok(map)
}

/// An injected header that [`InjectedHeader::new`] accepted could not be turned into a header
/// value: the two checks disagree, and nothing is sent.
fn unbuildable_header() -> Refusal {
    Refusal::new("502 Bad Gateway", "an injected header could not be built")
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod h2_tests;
