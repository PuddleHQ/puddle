// SPDX-License-Identifier: GPL-3.0-or-later
//! The certificate distribution points a workspace may fetch without a rule: the plain-HTTP
//! URLs that the host's synced roots name for checking a chain (CRL distribution points, the
//! issuer's certificate, OCSP responders). Pure matching; which addresses qualify is
//! [`crate::NetPolicy::pki_address`].

use puddle_types::Host;

use crate::normalise_host;

/// One URL a root names, split into what a request has to match.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    host: Host,
    port: u16,
    /// The URL's path: a request's path equals it, or lies below it as a directory.
    path: String,
}

/// The plain-HTTP URLs of certificate distribution points and authority information access, from
/// the roots the host synced. Empty by default: nothing is let through.
///
/// ```
/// use puddle_netpolicy::PkiDistributionPoints;
/// use puddle_types::Host;
///
/// let points = PkiDistributionPoints::from_urls(["http://pki.corp.example:8080/crl/root.crl"]);
/// let host = Host::parse_normalised("pki.corp.example").unwrap();
/// assert!(points.covers(&host, 8080, "GET", "/crl/root.crl"));
/// assert!(!points.covers(&host, 8080, "POST", "/crl/root.crl"));
/// assert!(!points.covers(&host, 80, "GET", "/crl/root.crl"));
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PkiDistributionPoints {
    entries: Vec<Entry>,
}

impl PkiDistributionPoints {
    /// The points named by `urls`. An entry that is not an `http://` URL with a plain host (no
    /// user information, a valid port) is left out, so one bad certificate extension names
    /// nothing.
    #[must_use]
    pub fn from_urls<I, S>(urls: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut entries: Vec<Entry> = urls
            .into_iter()
            .filter_map(|url| parse(url.as_ref()))
            .collect();
        entries.sort_by(|a, b| {
            (a.host.to_string(), a.port, &a.path).cmp(&(b.host.to_string(), b.port, &b.path))
        });
        entries.dedup();
        Self { entries }
    }

    /// Whether nothing is named.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// How many distinct URLs are named.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether a plain-HTTP request is for a named URL: only `GET` and `HEAD`, the same host and
    /// port, and a `path` (query and fragment already cut) that is the URL's path or below it.
    /// An OCSP responder is asked by a `GET` that appends the request to the path, which is the
    /// "below it" case.
    #[must_use]
    pub fn covers(&self, host: &Host, port: u16, method: &str, path: &str) -> bool {
        if !method.eq_ignore_ascii_case("GET") && !method.eq_ignore_ascii_case("HEAD") {
            return false;
        }
        self.entries
            .iter()
            .any(|e| e.host == *host && e.port == port && below(path, &e.path))
    }
}

/// Whether `path` is `base` or lies under it as a directory (`/crl/a` is under `/crl`, `/crl.evil`
/// is not).
fn below(path: &str, base: &str) -> bool {
    match path.strip_prefix(base) {
        Some("") => true,
        Some(rest) => base.ends_with('/') || rest.starts_with('/'),
        None => false,
    }
}

fn parse(url: &str) -> Option<Entry> {
    let rest = url
        .get(..7)
        .filter(|scheme| scheme.eq_ignore_ascii_case("http://"))
        .and_then(|_| url.get(7..))?;
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(end);
    let (name, port) = match authority.strip_prefix('[') {
        Some(inner) => {
            let (v6, after) = inner.split_once(']')?;
            let port = match after {
                "" => None,
                other => Some(other.strip_prefix(':')?),
            };
            (format!("[{v6}]"), port)
        }
        None => match authority.rsplit_once(':') {
            Some((name, port)) => (name.to_owned(), Some(port)),
            None => (authority.to_owned(), None),
        },
    };
    let port = match port {
        None => 80,
        Some(digits) => digits.parse().ok().filter(|&p| p != 0)?,
    };
    let host = normalise_host(&name).ok()?.into_host();
    let path = tail
        .find(['?', '#'])
        .and_then(|cut| tail.get(..cut))
        .unwrap_or(tail);
    let path = if path.is_empty() { "/" } else { path };
    Some(Entry {
        host,
        port,
        path: path.to_owned(),
    })
}

#[cfg(test)]
mod tests;
