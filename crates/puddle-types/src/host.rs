// SPDX-License-Identifier: AGPL-3.0-or-later
//! Normalised egress destinations.
//!
//! The proxy normalises what a guest asks for; everything downstream, the rules engine
//! included, accepts only these validated types and never parses raw input itself
//! (`docs/spec/rules.md` §1).

use std::fmt;
use std::net::IpAddr;

use crate::ValidationError;

/// Longest DNS name Puddle accepts, in bytes, without a trailing dot.
pub const MAX_NAME_LEN: usize = 253;
/// Longest DNS label, in bytes.
pub const MAX_LABEL_LEN: usize = 63;

const WHAT_NAME: &str = "host name";
const WHAT_IP: &str = "ip literal";

/// A DNS name in Puddle's normal form: lower-case ASCII (IDNA already applied), LDH labels of
/// 1 to 63 bytes, no trailing dot, at most 253 bytes, last label not all digits (such a name
/// reads as an IPv4 address to many resolvers).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DomainName(String);

impl DomainName {
    /// Accepts `name` only if it is already normalised; never rewrites it.
    ///
    /// # Errors
    /// A [`ValidationError`] naming the first rule `name` breaks.
    pub fn parse_normalised(name: &str) -> Result<Self, ValidationError> {
        let fail = |reason: &str| ValidationError::new(WHAT_NAME, name, reason);
        if name.is_empty() {
            return Err(fail("must not be empty"));
        }
        if name.len() > MAX_NAME_LEN {
            return Err(fail("must be at most 253 characters"));
        }
        let mut last = "";
        for label in name.split('.') {
            check_label(label).map_err(fail)?;
            last = label;
        }
        if last.bytes().all(|b| b.is_ascii_digit()) {
            return Err(fail("must not end in an all-numeric label"));
        }
        Ok(Self(name.to_owned()))
    }

    /// The name as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The labels from left to right.
    pub fn labels(&self) -> impl Iterator<Item = &str> {
        self.0.split('.')
    }
}

fn check_label(label: &str) -> Result<(), &'static str> {
    let bytes = label.as_bytes();
    let (Some(first), Some(last)) = (bytes.first(), bytes.last()) else {
        return Err("must not have an empty label");
    };
    if bytes.len() > MAX_LABEL_LEN {
        return Err("must not have a label longer than 63 characters");
    }
    let ldh = |b: &u8| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-';
    if !bytes.iter().all(ldh) || *first == b'-' || *last == b'-' {
        return Err("may only contain a-z, 0-9 and inner '-' per label");
    }
    Ok(())
}

impl fmt::Display for DomainName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A destination as the proxy normalised it: a [`DomainName`] or a canonical IP literal.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Host {
    /// A DNS name.
    Name(DomainName),
    /// An IP literal (IPv4 dotted quad, or IPv6 per RFC 5952 without brackets).
    Ip(IpAddr),
}

impl Host {
    /// Accepts `text` only if it is already a normalised name or a canonical IP literal.
    ///
    /// ```
    /// use puddle_types::Host;
    /// assert!(Host::parse_normalised("api.example.com").is_ok());
    /// assert!(Host::parse_normalised("2001:db8::1").is_ok());
    /// assert!(Host::parse_normalised("API.example.com").is_err());
    /// assert!(Host::parse_normalised("2001:DB8:0::1").is_err());
    /// ```
    ///
    /// # Errors
    /// A [`ValidationError`] when `text` is neither.
    pub fn parse_normalised(text: &str) -> Result<Self, ValidationError> {
        if let Ok(ip) = text.parse::<IpAddr>() {
            return if ip.to_string() == text {
                Ok(Self::Ip(ip))
            } else {
                Err(ValidationError::new(
                    WHAT_IP,
                    text,
                    "must be in canonical form",
                ))
            };
        }
        if text.contains(':') {
            return Err(ValidationError::new(
                WHAT_IP,
                text,
                "must be in canonical form",
            ));
        }
        DomainName::parse_normalised(text).map(Self::Name)
    }

    /// The name, or `None` for an IP literal.
    #[must_use]
    pub fn name(&self) -> Option<&DomainName> {
        match self {
            Self::Name(name) => Some(name),
            Self::Ip(_) => None,
        }
    }
}

impl fmt::Display for Host {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Name(name) => name.fmt(f),
            Self::Ip(ip) => ip.fmt(f),
        }
    }
}

#[cfg(test)]
mod tests;
