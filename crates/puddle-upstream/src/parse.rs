// SPDX-License-Identifier: AGPL-3.0-or-later
//! Readers for the text formats proxies are described in: PAC answers, the WinINet
//! `ProxyRules` value, and the `ProxyOverride` bypass list.

use std::net::IpAddr;

use crate::hop::{Destination, Hop, ProxyAddr, Scheme};

/// A PAC answer read into hops.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PacAnswer {
    /// The hops Puddle can use, in order.
    pub hops: Vec<Hop>,
    /// Entries left out because Puddle cannot speak them (`SOCKS`, `HTTPS` proxies, garbage).
    pub skipped: usize,
}

/// Reads `PROXY a:1; PROXY b:2; DIRECT`. `HTTP` is `PROXY`; `HTTPS`, `SOCKS*` and unknown entries
/// are counted in [`PacAnswer::skipped`].
#[must_use]
pub fn parse_pac_answer(text: &str) -> PacAnswer {
    let mut answer = PacAnswer::default();
    for entry in text.split(';').map(str::trim).filter(|e| !e.is_empty()) {
        let mut words = entry.split_whitespace();
        let hop = match (
            words.next().map(str::to_ascii_uppercase).as_deref(),
            words.next(),
        ) {
            (Some("DIRECT"), _) => Some(Hop::Direct),
            (Some("PROXY" | "HTTP"), Some(target)) => ProxyAddr::parse(target).ok().map(Hop::Proxy),
            _ => None,
        };
        match hop {
            Some(hop) => answer.hops.push(hop),
            None => answer.skipped += 1,
        }
    }
    answer
}

/// Which proxy serves which scheme. Read from the common list syntax (`host:port` for every
/// scheme, or `http=h:p;https=h:p;ftp=h:p;socks=h:p`, as WinINet and WinHTTP store it and as a
/// Puddle setting may spell it), or built from environment variables.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProxyRules {
    all: Option<ProxyAddr>,
    http: Option<ProxyAddr>,
    https: Option<ProxyAddr>,
}

impl ProxyRules {
    /// Rules from explicit proxies: `http` and `https` for their scheme, `all` for the rest.
    #[must_use]
    pub fn new(http: Option<ProxyAddr>, https: Option<ProxyAddr>, all: Option<ProxyAddr>) -> Self {
        Self { all, http, https }
    }

    /// True when no scheme has a proxy.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.all.is_none() && self.http.is_none() && self.https.is_none()
    }

    /// Reads the list syntax. Entries Puddle cannot use (`ftp=`, `socks=`, bad ports) are ignored;
    /// [`ProxyRules::parse_with_problems`] says which and why.
    #[must_use]
    pub fn parse(text: &str) -> Self {
        Self::parse_with_problems(text).0
    }

    /// Reads the list syntax like [`ProxyRules::parse`], and says what it left out: one text per
    /// `http=`, `https=` or bare entry that is not a usable proxy address, and one when the list
    /// names no usable proxy at all (it holds only SOCKS or FTP proxies, or nothing readable).
    /// `ftp=` and `socks=` entries next to a usable one are not problems: a WinINet list names
    /// them as a matter of course and Puddle only proxies HTTP and HTTPS. The texts carry no
    /// credential.
    #[must_use]
    pub fn parse_with_problems(text: &str) -> (Self, Vec<String>) {
        let mut server = Self::default();
        let mut problems = Vec::new();
        let mut note = |entry: &str, err: crate::hop::ParseError| {
            problems.push(format!("\"{}\": {err}", crate::redact::entry_text(entry)));
        };
        for entry in text
            .split([';', ' '])
            .map(str::trim)
            .filter(|e| !e.is_empty())
        {
            match entry.split_once('=') {
                None => match ProxyAddr::parse(entry) {
                    Ok(addr) => server.all = server.all.take().or(Some(addr)),
                    Err(err) => note(entry, err),
                },
                Some((key, value)) => {
                    let slot = match key.to_ascii_lowercase().as_str() {
                        "http" => &mut server.http,
                        "https" => &mut server.https,
                        _ => continue,
                    };
                    match ProxyAddr::parse(value) {
                        Ok(addr) => *slot = Some(addr),
                        Err(err) => note(entry, err),
                    }
                }
            }
        }
        if server.is_empty() && problems.is_empty() && !text.trim().is_empty() {
            problems.push(format!(
                "\"{}\" names no HTTP or HTTPS proxy (SOCKS and FTP proxies are not supported)",
                crate::redact::entry_text(text)
            ));
        }
        (server, problems)
    }

    /// The proxy for `scheme`, if the value names one.
    #[must_use]
    pub fn for_scheme(&self, scheme: Scheme) -> Option<&ProxyAddr> {
        let specific = match scheme {
            Scheme::Http => &self.http,
            Scheme::Https => &self.https,
        };
        specific.as_ref().or(self.all.as_ref())
    }
}

/// One `ProxyOverride` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Entry {
    /// `<local>`: a name without a dot.
    Local,
    /// `*` in `NO_PROXY`: everything.
    All,
    /// A `NO_PROXY` domain: the name itself and every subdomain, optionally on one port.
    Suffix { domain: String, port: Option<u16> },
    /// A `NO_PROXY` address range.
    Cidr { net: IpAddr, bits: u8 },
    /// A host pattern, `*` matching any run of characters, with an optional scheme and port.
    Pattern {
        scheme: Option<Scheme>,
        glob: String,
        port: Option<u16>,
        /// A leading-dot entry (`.corp.test`) also matches the bare domain.
        bare: Option<String>,
    },
}

/// The `ProxyOverride` list: destinations that skip the proxy. Entries are separated by `;`,
/// spaces or commas. Matching is case-insensitive; `*` is a wildcard anywhere (`*.corp.test`,
/// `10.*`); `<local>` is any name without a dot; `https://host` and `host:port` narrow an entry.
/// A leading dot (`.corp.test`) matches the domain and its subdomains, as the lab's PoC did.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BypassList {
    entries: Vec<Entry>,
}

impl BypassList {
    /// Reads the list.
    #[must_use]
    pub fn parse(text: &str) -> Self {
        let entries = text
            .split([';', ' ', ','])
            .map(str::trim)
            .filter(|e| !e.is_empty())
            .filter_map(parse_entry)
            .collect();
        Self { entries }
    }

    /// Reads the `NO_PROXY` syntax: comma separated; `*` is everything; `corp.test`, `.corp.test`
    /// and `*.corp.test` cover the domain and its subdomains; `host:port` narrows to a port;
    /// `10.0.0.0/8` is an address range.
    #[must_use]
    pub fn parse_no_proxy(text: &str) -> Self {
        let entries = text
            .split(',')
            .map(|e| e.trim().to_ascii_lowercase())
            .filter(|e| !e.is_empty())
            .filter_map(|e| parse_no_proxy_entry(&e))
            .collect();
        Self { entries }
    }

    /// How many entries the list has.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when the list has no entry.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// True when `dest` skips the proxy.
    #[must_use]
    pub fn matches(&self, dest: &Destination) -> bool {
        self.entries.iter().any(|entry| match entry {
            Entry::All => true,
            Entry::Suffix { domain, port } => {
                port.is_none_or(|p| p == dest.port())
                    && (dest.host() == domain
                        || dest
                            .host()
                            .strip_suffix(domain.as_str())
                            .is_some_and(|rest| rest.ends_with('.')))
            }
            Entry::Cidr { net, bits } => dest
                .host()
                .parse::<IpAddr>()
                .is_ok_and(|ip| in_cidr(*net, *bits, ip)),
            Entry::Local => {
                !dest.host().contains('.') && dest.host().parse::<std::net::Ipv6Addr>().is_err()
            }
            Entry::Pattern {
                scheme,
                glob,
                port,
                bare,
            } => {
                scheme.is_none_or(|s| s == dest.scheme())
                    && port.is_none_or(|p| p == dest.port())
                    && (glob_match(glob, dest.host()) || bare.as_deref() == Some(dest.host()))
            }
        })
    }
}

fn parse_no_proxy_entry(entry: &str) -> Option<Entry> {
    if entry == "*" {
        return Some(Entry::All);
    }
    if let Some((net, bits)) = entry.split_once('/') {
        let (net, bits) = (net.parse::<IpAddr>().ok()?, bits.parse::<u8>().ok()?);
        let max = if net.is_ipv4() { 32 } else { 128 };
        return (bits <= max).then_some(Entry::Cidr { net, bits });
    }
    let (host, port) = match entry.rsplit_once(':') {
        Some((host, port)) if !host.contains(':') => (host, Some(port.parse::<u16>().ok()?)),
        _ => (entry, None),
    };
    let domain = host.trim_start_matches("*.").trim_start_matches('.');
    (!domain.is_empty()).then(|| Entry::Suffix {
        domain: domain.to_string(),
        port,
    })
}

fn in_cidr(net: IpAddr, bits: u8, ip: IpAddr) -> bool {
    match (net, ip) {
        (IpAddr::V4(n), IpAddr::V4(i)) => {
            let mask = u32::MAX.checked_shl(32 - u32::from(bits)).unwrap_or(0);
            u32::from(n) & mask == u32::from(i) & mask
        }
        (IpAddr::V6(n), IpAddr::V6(i)) => {
            let mask = u128::MAX.checked_shl(128 - u32::from(bits)).unwrap_or(0);
            u128::from(n) & mask == u128::from(i) & mask
        }
        _ => false,
    }
}

fn parse_entry(raw: &str) -> Option<Entry> {
    let lower = raw.to_ascii_lowercase();
    if lower == "<local>" {
        return Some(Entry::Local);
    }
    if lower.starts_with('<') {
        return None; // `<-loopback>` and friends: loopback never uses a proxy here anyway
    }
    let (scheme, rest) = match lower.split_once("://") {
        Some(("http", rest)) => (Some(Scheme::Http), rest),
        Some(("https", rest)) => (Some(Scheme::Https), rest),
        Some(_) => return None,
        None => (None, lower.as_str()),
    };
    let rest = rest.trim_end_matches('/');
    let (host, port) = match rest.rsplit_once(':') {
        Some((host, port)) if !host.contains(':') => (host, Some(port.parse::<u16>().ok()?)),
        _ => (rest, None),
    };
    let host = host.trim_matches(['[', ']']);
    if host.is_empty() {
        return None;
    }
    let (glob, bare) = match host.strip_prefix('.') {
        Some(domain) => (format!("*.{domain}"), Some(domain.to_string())),
        None => (host.to_string(), None),
    };
    Some(Entry::Pattern {
        scheme,
        glob,
        port,
        bare,
    })
}

/// `*` matches any run of characters (including none); everything else matches itself.
#[must_use]
pub(crate) fn glob_match(pattern: &str, text: &str) -> bool {
    let (p, t): (Vec<char>, Vec<char>) = (pattern.chars().collect(), text.chars().collect());
    let (mut pi, mut ti) = (0, 0);
    let (mut star, mut mark) = (None, 0);
    while ti < t.len() {
        match p.get(pi) {
            Some('*') => {
                star = Some(pi);
                mark = ti;
                pi += 1;
            }
            Some(c) if t.get(ti) == Some(c) => {
                pi += 1;
                ti += 1;
            }
            _ => match star {
                Some(s) => {
                    pi = s + 1;
                    mark += 1;
                    ti = mark;
                }
                None => return false,
            },
        }
    }
    p.get(pi..)
        .is_some_and(|rest| rest.iter().all(|c| *c == '*'))
}

#[cfg(test)]
mod tests;
