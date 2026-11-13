// SPDX-License-Identifier: AGPL-3.0-or-later
//! The name stage: turn the host a guest asked for into Puddle's normal form, before any rule
//! lookup and before DNS (`docs/spec/rules.md` §1).
//!
//! Strict on purpose: what comes out is either a canonical IP literal or a [`DomainName`]
//! (lower-case ASCII, IDNA applied, LDH labels, no trailing dot, at most 253 bytes). Anything a
//! resolver could read in two ways is refused rather than guessed at.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use idna::uts46::{AsciiDenyList, DnsLength, Hyphens, Uts46};
use puddle_types::{DomainName, Host, LocalCategory};

/// Longest raw host accepted, in bytes, before any mapping. Bounds the IDNA work a hostile guest
/// can cause; a valid name is at most 253 bytes, and its Unicode form at most a few times that.
pub const MAX_RAW_HOST_LEN: usize = 1024;

/// Longest prefix of a refused host kept in a [`NameError`] (it came from the guest).
const MAX_ECHOED_CHARS: usize = 64;

/// Names that mean "the metadata service" wherever they are resolved.
const METADATA_NAMES: [&str; 5] = [
    "metadata",
    "metadata.google.internal",
    "metadata.goog",
    "instance-data",
    "instance-data.ec2.internal",
];

/// A normalised destination, plus the local category its *name* alone puts it in.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Target {
    host: Host,
    named: Option<LocalCategory>,
}

impl Target {
    /// The normalised host, what rules match on.
    #[must_use]
    pub fn host(&self) -> &Host {
        &self.host
    }

    /// The category a name is in by itself: [`LocalCategory::Loopback`] for `localhost` and
    /// `*.localhost`, [`LocalCategory::Metadata`] for the cloud metadata names. `None` for IP
    /// literals (classify those) and ordinary names (classify what they resolve to).
    #[must_use]
    pub fn named_category(&self) -> Option<LocalCategory> {
        self.named
    }

    /// The target for a host that is already normalised (from a rule or a stored request).
    #[must_use]
    pub fn from_host(host: Host) -> Self {
        let named = host.name().and_then(|n| named_category(n.as_str()));
        Self { host, named }
    }

    /// The target as a [`Host`].
    #[must_use]
    pub fn into_host(self) -> Host {
        self.host
    }
}

/// The guest's host could not be normalised. The proxy answers with a client error (400) or, for
/// a non-canonical number, a refusal that says which address it decodes to.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum NameError {
    /// A number that platform resolvers would read as an IPv4 address (`127.1`, `0x7f000001`,
    /// `0177.0.0.1`). Refused as a matter of form: written canonically, it falls under that
    /// address's category like any literal.
    #[error("{input:?} is not a canonical IP address{}", decoded_hint(*.decoded))]
    NonCanonicalIp {
        /// The refused host (truncated, from the guest).
        input: String,
        /// The address it decodes to, if it decodes at all.
        decoded: Option<Ipv4Addr>,
    },
    /// Not a host name or IP literal.
    #[error("invalid host {input:?}: {reason}")]
    Invalid {
        /// The refused host (truncated, from the guest).
        input: String,
        /// Which rule it breaks.
        reason: &'static str,
    },
}

fn decoded_hint(decoded: Option<Ipv4Addr>) -> String {
    decoded.map_or_else(
        || ": it reads as a number but decodes to no address".to_owned(),
        |ip| format!(": it decodes to {ip}, write that instead"),
    )
}

fn echo(raw: &str) -> String {
    let mut out: String = raw.chars().take(MAX_ECHOED_CHARS).collect();
    if raw.chars().count() > MAX_ECHOED_CHARS {
        out.push('…');
    }
    out
}

fn invalid(raw: &str, reason: &'static str) -> NameError {
    NameError::Invalid {
        input: echo(raw),
        reason,
    }
}

/// Normalises the host part of a request target (`CONNECT` authority or absolute-form URL, the
/// port already split off; IPv6 may still be in brackets).
///
/// - IPv6 literals are parsed with or without brackets and written per RFC 5952; zone IDs are
///   refused.
/// - Names are mapped with UTS #46 (non-transitional, STD3 ASCII rules: upper case folded,
///   full-width characters mapped, Unicode to `xn--` Punycode), then must be LDH labels.
///   One trailing dot is dropped.
/// - A name whose last label is a number (WHATWG "ends in a number") is an IPv4 address: the
///   canonical dotted quad is accepted, any other spelling is [`NameError::NonCanonicalIp`].
///
/// ```
/// use puddle_netpolicy::normalise_host;
/// use puddle_types::LocalCategory;
///
/// assert_eq!(normalise_host("Bücher.Example.").unwrap().host().to_string(), "xn--bcher-kva.example");
/// assert_eq!(normalise_host("[2001:DB8::1]").unwrap().host().to_string(), "2001:db8::1");
/// assert_eq!(normalise_host("LOCALHOST").unwrap().named_category(), Some(LocalCategory::Loopback));
/// assert!(normalise_host("127.1").is_err());
/// assert!(normalise_host("user@example.com").is_err());
/// ```
///
/// # Errors
/// [`NameError::NonCanonicalIp`] for a non-canonical number, [`NameError::Invalid`] for anything
/// else that isn't a host.
pub fn normalise_host(raw: &str) -> Result<Target, NameError> {
    if raw.is_empty() {
        return Err(invalid(raw, "is empty"));
    }
    if raw.len() > MAX_RAW_HOST_LEN {
        return Err(invalid(raw, "is too long"));
    }
    if let Some(inner) = raw.strip_prefix('[') {
        let inner = inner
            .strip_suffix(']')
            .ok_or_else(|| invalid(raw, "has an unclosed '['"))?;
        return parse_v6(raw, inner);
    }
    if raw.contains(':') {
        return parse_v6(raw, raw);
    }
    let ascii = Uts46::new()
        .to_ascii(
            raw.as_bytes(),
            AsciiDenyList::STD3,
            Hyphens::Allow,
            DnsLength::Ignore,
        )
        .map_err(|_| invalid(raw, "is not a valid host name (IDNA/LDH rules)"))?;
    let name = ascii.strip_suffix('.').unwrap_or(&ascii);
    if ends_in_a_number(name) {
        return match parse_ipv4_number(name) {
            Some(ip) if ip.to_string() == name => Ok(literal(IpAddr::V4(ip))),
            decoded => Err(NameError::NonCanonicalIp {
                input: echo(raw),
                decoded,
            }),
        };
    }
    let name = DomainName::parse_normalised(name).map_err(|e| NameError::Invalid {
        input: echo(raw),
        reason: name_rule(e.reason()),
    })?;
    let named = named_category(name.as_str());
    Ok(Target {
        host: Host::Name(name),
        named,
    })
}

fn literal(ip: IpAddr) -> Target {
    Target {
        host: Host::Ip(ip),
        named: None,
    }
}

fn parse_v6(raw: &str, text: &str) -> Result<Target, NameError> {
    if text.contains('%') {
        return Err(invalid(raw, "has an IPv6 zone ID"));
    }
    text.parse::<Ipv6Addr>()
        .map(|ip| literal(IpAddr::V6(ip)))
        .map_err(|_| invalid(raw, "is not a valid IPv6 address"))
}

/// `DomainName` reasons are `String`s; map the ones it can give to static text (it validates
/// what IDNA produced, so only length and empty labels are reachable here).
fn name_rule(reason: &str) -> &'static str {
    if reason.contains("253") {
        "is longer than 253 characters"
    } else if reason.contains("63") {
        "has a label longer than 63 characters"
    } else if reason.contains("empty") {
        "has an empty label"
    } else {
        "is not a valid host name"
    }
}

fn named_category(name: &str) -> Option<LocalCategory> {
    if name == "localhost" || name.ends_with(".localhost") {
        Some(LocalCategory::Loopback)
    } else if METADATA_NAMES.contains(&name) {
        Some(LocalCategory::Metadata)
    } else {
        None
    }
}

/// WHATWG URL "ends in a number": the last label is all digits, or `0x` followed by hex digits.
fn ends_in_a_number(name: &str) -> bool {
    let last = name.rsplit('.').next().unwrap_or(name);
    if !last.is_empty() && last.bytes().all(|b| b.is_ascii_digit()) {
        return true;
    }
    last.strip_prefix("0x")
        .is_some_and(|hex| hex.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// WHATWG / `inet_aton` IPv4 parsing: 1 to 4 parts, each decimal, `0`-prefixed octal or
/// `0x`-prefixed hex; the last part fills the remaining bytes. `None` if it isn't one.
fn parse_ipv4_number(name: &str) -> Option<Ipv4Addr> {
    let parts: Vec<&str> = name.split('.').collect();
    if parts.len() > 4 {
        return None;
    }
    let numbers = parts
        .iter()
        .map(|p| parse_ipv4_part(p))
        .collect::<Option<Vec<u64>>>()?;
    let (last, head) = numbers.split_last()?;
    if head.iter().any(|n| *n > 255) {
        return None;
    }
    let free_bits = 8 * (4 - head.len());
    if *last >= 1u64 << free_bits {
        return None;
    }
    let mut value = *last;
    for (i, n) in head.iter().enumerate() {
        value |= n << (24 - 8 * i);
    }
    u32::try_from(value).ok().map(Ipv4Addr::from_bits)
}

fn parse_ipv4_part(part: &str) -> Option<u64> {
    if let Some(hex) = part.strip_prefix("0x") {
        return if hex.is_empty() {
            Some(0)
        } else {
            u64::from_str_radix(hex, 16).ok()
        };
    }
    match part.strip_prefix('0') {
        Some(octal) if !octal.is_empty() => u64::from_str_radix(octal, 8).ok(),
        _ => part.parse().ok(),
    }
}

#[cfg(test)]
mod tests;
