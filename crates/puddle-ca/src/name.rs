// SPDX-License-Identifier: AGPL-3.0-or-later
//! The host names a leaf certificate is issued for.

use std::fmt;

const MAX_DNS_LEN: usize = 253;
const MAX_LABEL_LEN: usize = 63;

/// A validated, normalised DNS name: lower case, no trailing dot. An IP address is not one, so the
/// CA never certifies an address.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct DnsName(String);

impl DnsName {
    /// A lower-cased DNS name without its trailing dot. `None` for anything else: an IP address
    /// (bracketed or not), a wildcard, an empty label.
    pub(crate) fn parse(input: &str) -> Option<Self> {
        let name = input
            .strip_suffix('.')
            .unwrap_or(input)
            .to_ascii_lowercase();
        valid_dns(&name).then_some(Self(name))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DnsName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Lower-case letters, digits, `-` and `_` in labels of 1–63 bytes, no label starting or ending
/// with `-`, at most 253 bytes, and not all-numeric in the last label (so `1.2.3.4`, `1.2.3.4.5`
/// and other strings that look like addresses are refused).
fn valid_dns(name: &str) -> bool {
    if name.is_empty() || name.len() > MAX_DNS_LEN {
        return false;
    }
    let labels_ok = name.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= MAX_LABEL_LEN
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
    });
    let last_not_numeric = name
        .rsplit('.')
        .next()
        .is_some_and(|last| !last.bytes().all(|b| b.is_ascii_digit()));
    labels_ok && last_not_numeric
}

#[cfg(test)]
mod tests;
