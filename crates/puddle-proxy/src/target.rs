// SPDX-License-Identifier: AGPL-3.0-or-later
//! The destination of a request, normalised into a [`Host`] the rules engine accepts
//! (`puddle_netpolicy::normalise_host`: lower case, IDNA to ASCII, LDH labels, canonical IP
//! literals; anything else is a `400`).

use std::net::IpAddr;

use puddle_types::Host;

/// A normalised destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Target {
    /// The host.
    pub(crate) host: Host,
    /// The port.
    pub(crate) port: u16,
}

impl Target {
    /// The `Host` header value for an absolute-form request: the authority, without the port when
    /// it is 80, with brackets around an IPv6 literal.
    pub(crate) fn host_header(&self) -> String {
        let host = match &self.host {
            Host::Ip(IpAddr::V6(ip)) => format!("[{ip}]"),
            Host::Name(_) | Host::Ip(IpAddr::V4(_)) => self.host.to_string(),
        };
        if self.port == 80 {
            host
        } else {
            format!("{host}:{}", self.port)
        }
    }
}

#[cfg(test)]
mod tests;
