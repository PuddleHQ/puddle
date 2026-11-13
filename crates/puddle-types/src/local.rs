// SPDX-License-Identifier: AGPL-3.0-or-later
//! Local-destination categories: the address classes a user can switch on per workspace.
//!
//! One enum for every crate: `puddle-netpolicy` classifies addresses into it, `puddle-settings`
//! stores one toggle per category under [`LocalCategory::key`], the proxy's block reason and the
//! audit's `toggle:<key>` name it. The keys are stored format: renaming one needs a settings
//! migration.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::ValidationError;

/// A class of local destination with its own toggle. Everything outside these is public.
///
/// A toggle only *permits* its category; each destination still needs an allow rule or an
/// approval. Puddle's own endpoints are not a category: no toggle reaches them.
///
/// Non-exhaustive: more categories may follow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[non_exhaustive]
pub enum LocalCategory {
    /// The host itself: `127.0.0.0/8`, `::1`, the unspecified addresses `0.0.0.0` and `::`
    /// (connecting to them reaches the host's own listeners), `localhost` and `*.localhost`.
    Loopback,
    /// Local and private networks: RFC 1918, CGNAT `100.64.0.0/10`, unique-local `fc00::/7`.
    Private,
    /// Link-local `169.254.0.0/16` and `fe80::/10`, apart from the metadata addresses in them.
    LinkLocal,
    /// Cloud instance metadata and credential endpoints (`169.254.169.254`, Azure's
    /// `168.63.129.16`, `metadata.google.internal`, ...).
    Metadata,
    /// Addresses with no business being a TCP destination: multicast, broadcast, reserved,
    /// documentation, benchmarking, "this network", IPv6 outside `2000::/3`, and transition
    /// prefixes (6to4, Teredo, IPv4-compatible) that embed a public address.
    Special,
    /// The company's own network as this host sees it: names the proxy script sends direct or
    /// that sit under the machine's DNS suffixes, a private-access client's `100.64.0.0/10`,
    /// private ranges routed through a VPN. Never loopback, link-local, metadata or special-use
    /// addresses, and never an address on the physical LAN's own subnet. Off by default.
    CompanyNetwork,
}

impl LocalCategory {
    /// Every category, in a fixed order (the order block messages list them in).
    pub const ALL: [Self; 6] = [
        Self::Loopback,
        Self::Private,
        Self::LinkLocal,
        Self::Metadata,
        Self::Special,
        Self::CompanyNetwork,
    ];

    /// The toggle's key: in settings documents, the API, and the audit's `toggle:<key>`.
    ///
    /// ```
    /// use puddle_types::LocalCategory;
    /// assert_eq!(LocalCategory::LinkLocal.key(), "link_local");
    /// assert_eq!("link_local".parse(), Ok(LocalCategory::LinkLocal));
    /// ```
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Loopback => "loopback",
            Self::Private => "private",
            Self::LinkLocal => "link_local",
            Self::Metadata => "metadata",
            Self::Special => "special",
            Self::CompanyNetwork => "company_network",
        }
    }

    /// A short description for messages and the UI.
    #[must_use]
    pub const fn describe(self) -> &'static str {
        match self {
            Self::Loopback => "host loopback",
            Self::Private => "local/private network",
            Self::LinkLocal => "link-local",
            Self::Metadata => "cloud metadata",
            Self::Special => "special-use address",
            Self::CompanyNetwork => "company network",
        }
    }

    /// The audit `reason` for a request this category's toggle blocked (R-24): `toggle:<key>`.
    #[must_use]
    pub const fn audit_reason(self) -> &'static str {
        match self {
            Self::Loopback => "toggle:loopback",
            Self::Private => "toggle:private",
            Self::LinkLocal => "toggle:link_local",
            Self::Metadata => "toggle:metadata",
            Self::Special => "toggle:special",
            Self::CompanyNetwork => "toggle:company_network",
        }
    }
}

/// Which host signal put an address in [`LocalCategory::CompanyNetwork`]. The `code` is stored
/// in the audit's `signal` field, so the codes are stored format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[non_exhaustive]
pub enum CompanySignal {
    /// The proxy script answers `DIRECT` for the name while it names proxies for other URLs.
    PacDirect,
    /// The name is under the machine's primary or connection-specific DNS suffix, its suffix
    /// search list or a name-resolution-policy namespace.
    DnsSuffix,
    /// The address is in `100.64.0.0/10` and a known private-access client is installed.
    PrivateAccessCgnat,
    /// The address is in a private range that is routed through a VPN interface.
    VpnRoute,
}

impl CompanySignal {
    /// Every signal, in the order they are tried.
    pub const ALL: [Self; 4] = [
        Self::PacDirect,
        Self::DnsSuffix,
        Self::PrivateAccessCgnat,
        Self::VpnRoute,
    ];

    /// The audit `signal` code.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::PacDirect => "pac_direct",
            Self::DnsSuffix => "dns_suffix",
            Self::PrivateAccessCgnat => "private_access_cgnat",
            Self::VpnRoute => "vpn_route",
        }
    }

    /// How the destination was found, as the end of "reached through ...", for messages.
    #[must_use]
    pub const fn describe(self) -> &'static str {
        match self {
            Self::PacDirect => "your company's proxy settings, which send it direct",
            Self::DnsSuffix => "your company's DNS suffix",
            Self::PrivateAccessCgnat => "a private-access client",
            Self::VpnRoute => "your VPN",
        }
    }
}

impl fmt::Display for CompanySignal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

impl fmt::Display for LocalCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.key())
    }
}

impl FromStr for LocalCategory {
    type Err = ValidationError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|c| c.key() == s)
            .ok_or_else(|| ValidationError::new("local category", s, "is not a known category"))
    }
}

#[cfg(test)]
mod tests;
