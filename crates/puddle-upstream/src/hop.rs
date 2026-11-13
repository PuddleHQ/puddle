// SPDX-License-Identifier: AGPL-3.0-or-later
//! What a route is made of: proxy addresses, hops, and the destination being routed.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// Why a proxy address or a destination could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ParseError {
    /// Nothing there.
    #[error("empty proxy address")]
    Empty,
    /// The port is not a number in 1..=65535.
    #[error("bad port in proxy address")]
    BadPort,
    /// The host is empty or contains characters no host name has.
    #[error("bad host in proxy address")]
    BadHost,
    /// A scheme Puddle cannot speak to a proxy (`https`, `socks5`, ...).
    #[error("unsupported proxy scheme {0:?}")]
    UnsupportedScheme(String),
}

/// An HTTP proxy to send a request through: a host name or address and a port. Credentials are
/// never part of it (the auth layer supplies them at connect time).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ProxyAddr {
    host: String,
    port: u16,
}

impl ProxyAddr {
    /// A proxy at `host:port`. The host is lower-cased, and an IPv6 literal loses its brackets.
    #[must_use]
    pub fn new(host: &str, port: u16) -> Self {
        let host = host.trim().trim_matches(['[', ']']).to_ascii_lowercase();
        Self { host, port }
    }

    /// Reads `host`, `host:port`, `[v6]:port` or `http://host[:port][/]`. A missing port is 80.
    ///
    /// # Errors
    /// [`ParseError`] when the text is empty, has no usable host or port, or names a scheme other
    /// than `http`.
    pub fn parse(text: &str) -> Result<Self, ParseError> {
        let mut rest = text.trim();
        if let Some((scheme, after)) = rest.split_once("://") {
            if !scheme.eq_ignore_ascii_case("http") {
                return Err(ParseError::UnsupportedScheme(scheme.to_ascii_lowercase()));
            }
            rest = after;
        }
        rest = rest.trim_end_matches('/');
        // Credentials in a proxy URL are dropped: this type never carries them.
        rest = rest.rsplit_once('@').map_or(rest, |(_, host)| host);
        if rest.is_empty() {
            return Err(ParseError::Empty);
        }
        let (host, port) = split_host_port(rest)?;
        if host.is_empty()
            || host
                .chars()
                .any(|c| c.is_whitespace() || matches!(c, '/' | '\\' | '@' | ';' | ',' | '='))
        {
            return Err(ParseError::BadHost);
        }
        Ok(Self::new(host, port.unwrap_or(80)))
    }

    /// The host name or address, lower case, without brackets.
    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    /// The port.
    #[must_use]
    pub fn port(&self) -> u16 {
        self.port
    }
}

fn split_host_port(text: &str) -> Result<(&str, Option<u16>), ParseError> {
    let parse_port = |p: &str| match p.parse::<u16>() {
        Ok(0) | Err(_) => Err(ParseError::BadPort),
        Ok(port) => Ok(port),
    };
    if let Some(inner) = text.strip_prefix('[') {
        let (host, after) = inner.split_once(']').ok_or(ParseError::BadHost)?;
        return match after.strip_prefix(':') {
            Some(port) => Ok((host, Some(parse_port(port)?))),
            None if after.is_empty() => Ok((host, None)),
            None => Err(ParseError::BadHost),
        };
    }
    match text.rsplit_once(':') {
        // A second colon without brackets is a bare IPv6 literal, which has no port.
        Some((host, _)) if host.contains(':') => Ok((text, None)),
        Some((host, port)) => Ok((host, Some(parse_port(port)?))),
        None => Ok((text, None)),
    }
}

impl fmt::Display for ProxyAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.host.contains(':') {
            write!(f, "[{}]:{}", self.host, self.port)
        } else {
            write!(f, "{}:{}", self.host, self.port)
        }
    }
}

/// One step of a route: go straight to the destination, or through a proxy.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Hop {
    /// Connect to the destination itself.
    Direct,
    /// Send the request through an HTTP proxy (`CONNECT`, or the absolute-form request).
    Proxy(ProxyAddr),
}

impl fmt::Display for Hop {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Direct => f.write_str("DIRECT"),
            Self::Proxy(proxy) => write!(f, "PROXY {proxy}"),
        }
    }
}

/// An ordered, never-empty list of hops, PAC style: try the first, fall through to the next when
/// it cannot be reached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    hops: Vec<Hop>,
}

impl Route {
    /// A route over `hops`, or `None` when the list is empty.
    #[must_use]
    pub fn new(hops: Vec<Hop>) -> Option<Self> {
        (!hops.is_empty()).then_some(Self { hops })
    }

    /// Straight to the destination.
    #[must_use]
    pub fn direct() -> Self {
        Self {
            hops: vec![Hop::Direct],
        }
    }

    /// Through one proxy, with no fallback.
    #[must_use]
    pub fn via(proxy: ProxyAddr) -> Self {
        Self {
            hops: vec![Hop::Proxy(proxy)],
        }
    }

    /// The hops, in the order to try them.
    #[must_use]
    pub fn hops(&self) -> &[Hop] {
        &self.hops
    }

    /// True when the only hop is [`Hop::Direct`].
    #[must_use]
    pub fn is_direct(&self) -> bool {
        self.hops.iter().all(|hop| matches!(hop, Hop::Direct))
    }

    /// The same hops with every proxy for which `is_bad` holds moved behind the others (order
    /// kept inside each group). A route is never shortened: a proxy that looked dead may be the
    /// only way out.
    pub(crate) fn demote(&self, is_bad: impl Fn(&ProxyAddr) -> bool) -> Self {
        let (bad, good): (Vec<_>, Vec<_>) = self
            .hops
            .iter()
            .cloned()
            .partition(|hop| matches!(hop, Hop::Proxy(proxy) if is_bad(proxy)));
        Self {
            hops: good.into_iter().chain(bad).collect(),
        }
    }
}

impl fmt::Display for Route {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, hop) in self.hops.iter().enumerate() {
            if i > 0 {
                f.write_str("; ")?;
            }
            write!(f, "{hop}")?;
        }
        Ok(())
    }
}

/// The scheme of the request being routed. PAC scripts and proxy settings differ per scheme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Scheme {
    /// Plain HTTP (port 80 by default).
    Http,
    /// HTTPS, reaching the proxy as `CONNECT` (port 443 by default).
    Https,
}

impl Scheme {
    /// The scheme as it appears in a URL.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Https => "https",
        }
    }

    fn default_port(self) -> u16 {
        match self {
            Self::Http => 80,
            Self::Https => 443,
        }
    }
}

/// Where a request is going: what discovery decides a route for.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Destination {
    scheme: Scheme,
    host: String,
    port: u16,
}

impl Destination {
    /// A destination. The host is lower-cased, trailing dot removed, IPv6 brackets removed.
    #[must_use]
    pub fn new(scheme: Scheme, host: &str, port: u16) -> Self {
        let host = host
            .trim()
            .trim_matches(['[', ']'])
            .trim_end_matches('.')
            .to_ascii_lowercase();
        Self { scheme, host, port }
    }

    /// The scheme.
    #[must_use]
    pub fn scheme(&self) -> Scheme {
        self.scheme
    }

    /// The host, lower case, without brackets or a trailing dot.
    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    /// The port.
    #[must_use]
    pub fn port(&self) -> u16 {
        self.port
    }

    /// The URL handed to a PAC script: `scheme://host/`, with the port only when it isn't the
    /// scheme's default (as a browser would). No path or query: Puddle never sees them for a
    /// `CONNECT`, and must not log or send them for plain HTTP either.
    #[must_use]
    pub fn pac_url(&self) -> String {
        let host = if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        if self.port == self.scheme.default_port() {
            format!("{}://{host}/", self.scheme.as_str())
        } else {
            format!("{}://{host}:{}/", self.scheme.as_str(), self.port)
        }
    }

    /// True for names and addresses that mean this machine: `localhost`, `*.localhost`,
    /// `127.0.0.0/8`, `::1`, the unspecified addresses. These never go through a proxy.
    #[must_use]
    pub fn is_loopback(&self) -> bool {
        if self.host == "localhost" || self.host.ends_with(".localhost") {
            return true;
        }
        match self.host.parse::<IpAddr>() {
            Ok(IpAddr::V4(ip)) => ip.is_loopback() || ip == Ipv4Addr::UNSPECIFIED,
            Ok(IpAddr::V6(ip)) => {
                ip.is_loopback()
                    || ip == Ipv6Addr::UNSPECIFIED
                    || ip.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback())
            }
            Err(_) => false,
        }
    }
}

#[cfg(test)]
mod tests;
