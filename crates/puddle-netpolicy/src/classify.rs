// SPDX-License-Identifier: GPL-3.0-or-later
//! Address classes: which [`LocalCategory`] an IP address is in, or public.
//!
//! Fails closed: anything that is not plainly global unicast lands in a category. IPv4 embedded
//! in IPv6 (mapped, NAT64, IPv4-compatible, 6to4, Teredo) is classified by the embedded address,
//! so no spelling of a local address passes as public. Only stable `std` predicates are used;
//! the rest are CIDR checks written out here.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use puddle_types::{CompanySignal, Host, LocalCategory};

use crate::company::company_signal;
use crate::{CompanySignals, EndpointKind};

/// Which class a destination address is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum AddressClass {
    /// Public unicast: only the rules decide.
    Public,
    /// In a local category; its toggle decides whether rules may allow it.
    Local {
        /// The category.
        category: LocalCategory,
        /// Why, for the audit (`rfc1918`, `nat64`, `metadata`, ...).
        rule: &'static str,
    },
    /// A private address the host's signals put in the company network (a
    /// [`LocalCategory::CompanyNetwork`] destination). Only [`crate::NetPolicy::classify_named`]
    /// gives it, since it needs the host's signals.
    Company(CompanySignal),
    /// One of Puddle's own listeners. Not host loopback: no toggle reaches it, and
    /// a later per-workspace unblock would key on this class. Only
    /// [`crate::NetPolicy::classify`] gives it, since it needs the port and the registry.
    PuddleEndpoint(EndpointKind),
}

impl AddressClass {
    /// The local category, if the class is one.
    #[must_use]
    pub fn category(self) -> Option<LocalCategory> {
        match self {
            Self::Local { category, .. } => Some(category),
            Self::Company(_) => Some(LocalCategory::CompanyNetwork),
            Self::Public | Self::PuddleEndpoint(_) => None,
        }
    }

    /// The audit `rule` for this class: `public`, the local rule, the company-network signal, or
    /// `puddle_endpoint`.
    #[must_use]
    pub fn rule(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Local { rule, .. } => rule,
            Self::Company(signal) => signal.code(),
            Self::PuddleEndpoint(_) => "puddle_endpoint",
        }
    }
}

const METADATA_V4: [Ipv4Addr; 6] = [
    Ipv4Addr::new(169, 254, 169, 254), // AWS, Azure, GCP, OCI instance metadata
    Ipv4Addr::new(169, 254, 170, 2),   // AWS ECS task credentials
    Ipv4Addr::new(169, 254, 170, 23),  // AWS EKS pod identity
    Ipv4Addr::new(168, 63, 129, 16),   // Azure WireServer: public, no range rule catches it
    Ipv4Addr::new(100, 100, 100, 200), // Alibaba Cloud
    Ipv4Addr::new(192, 0, 0, 192),     // OCI
];

const METADATA_V6: [Ipv6Addr; 3] = [
    Ipv6Addr::new(0xfd00, 0x0ec2, 0, 0, 0, 0, 0, 0x0254), // AWS instance metadata
    Ipv6Addr::new(0xfd00, 0x0ec2, 0, 0, 0, 0, 0, 0x0023), // AWS EKS pod identity
    Ipv6Addr::new(0xfd20, 0x00ce, 0, 0, 0, 0, 0, 0x0254), // GCP
];

fn in4(a: Ipv4Addr, net: u32, bits: u32) -> bool {
    let mask = u32::MAX.checked_shl(32 - bits).unwrap_or(0);
    a.to_bits() & mask == net & mask
}

fn in6(a: Ipv6Addr, net: u128, bits: u32) -> bool {
    let mask = u128::MAX.checked_shl(128 - bits).unwrap_or(0);
    a.to_bits() & mask == net & mask
}

fn v4_from(bits: u128) -> Ipv4Addr {
    Ipv4Addr::from_bits(u32::try_from(bits & u128::from(u32::MAX)).unwrap_or(0))
}

const fn local(category: LocalCategory, rule: &'static str) -> AddressClass {
    AddressClass::Local { category, rule }
}

/// The class of `ip` by address alone (never [`AddressClass::PuddleEndpoint`]).
///
/// ```
/// use puddle_netpolicy::{AddressClass, classify_ip};
/// use puddle_types::LocalCategory;
///
/// assert_eq!(classify_ip("8.8.8.8".parse().unwrap()), AddressClass::Public);
/// assert_eq!(classify_ip("::ffff:10.0.0.1".parse().unwrap()).category(), Some(LocalCategory::Private));
/// ```
#[must_use]
pub fn classify_ip(ip: IpAddr) -> AddressClass {
    match ip {
        IpAddr::V4(a) => classify_v4(a),
        IpAddr::V6(a) => classify_v6(a),
    }
}

/// The class of `ip` by address and the host's company-network signals (never
/// [`AddressClass::PuddleEndpoint`]): [`classify_ip`], except that a private address the signals
/// put in the company network is [`AddressClass::Company`]. `name` is the host name the address
/// was reached by, if any (an IP literal has none).
#[must_use]
pub fn classify_with_signals(
    ip: IpAddr,
    name: Option<&Host>,
    signals: &dyn CompanySignals,
) -> AddressClass {
    let class = classify_ip(ip);
    let name = match name {
        Some(Host::Name(name)) => Some(name.as_str()),
        _ => None,
    };
    let private = class.category() == Some(LocalCategory::Private);
    match company_signal(private, ip, name, signals) {
        Some(signal) => AddressClass::Company(signal),
        None => class,
    }
}

fn classify_v4(a: Ipv4Addr) -> AddressClass {
    use LocalCategory::{LinkLocal, Loopback, Metadata, Private, Special};
    if METADATA_V4.contains(&a) {
        return local(Metadata, "metadata");
    }
    // Connecting to 0.0.0.0 reaches the host's own listeners on Linux.
    if a.is_unspecified() {
        return local(Loopback, "unspecified");
    }
    if in4(a, 0, 8) {
        return local(Special, "this-network");
    }
    if a.is_loopback() {
        return local(Loopback, "loopback");
    }
    if a.is_link_local() {
        return local(LinkLocal, "link-local");
    }
    if a.is_private() {
        return local(Private, "rfc1918");
    }
    if in4(a, 0x6440_0000, 10) {
        return local(Private, "cgnat");
    }
    if a.is_broadcast() {
        return local(Special, "broadcast");
    }
    if a.is_multicast() {
        return local(Special, "multicast");
    }
    if a.is_documentation() {
        return local(Special, "documentation");
    }
    if in4(a, 0xC612_0000, 15) {
        return local(Special, "benchmarking");
    }
    if in4(a, 0xC000_0000, 24) || in4(a, 0xC058_6300, 24) || in4(a, 0xF000_0000, 4) {
        return local(Special, "reserved");
    }
    AddressClass::Public
}

/// A transition prefix's embedded IPv4 address: a local one gives its category (with the
/// prefix's rule); a public one leaves the address special (6to4, Teredo and IPv4-compatible
/// need relays a workspace has no reason to use).
fn embedded(v4: Ipv4Addr, rule: &'static str) -> AddressClass {
    match classify_v4(v4) {
        AddressClass::Local { category, .. } => local(category, rule),
        AddressClass::Public | AddressClass::Company(_) | AddressClass::PuddleEndpoint(_) => {
            local(LocalCategory::Special, rule)
        }
    }
}

/// The embedded address's class, with the prefix's rule; public stays public.
fn translated(v4: Ipv4Addr, rule: &'static str) -> AddressClass {
    match classify_v4(v4) {
        AddressClass::Local { category, .. } => local(category, rule),
        other @ (AddressClass::Public
        | AddressClass::Company(_)
        | AddressClass::PuddleEndpoint(_)) => other,
    }
}

fn classify_v6(a: Ipv6Addr) -> AddressClass {
    use LocalCategory::{LinkLocal, Loopback, Metadata, Private, Special};
    let bits = a.to_bits();
    // Not `to_ipv4()`: that also maps ::1 to 0.0.0.1.
    if let Some(v4) = a.to_ipv4_mapped() {
        return translated(v4, "ipv4-mapped");
    }
    // NAT64 well-known prefix: a NAT64 network must still reach public IPv4.
    if in6(a, 0x0064_ff9b << 96, 96) {
        return translated(v4_from(bits), "nat64");
    }
    if a.is_unspecified() {
        return local(Loopback, "unspecified");
    }
    if a.is_loopback() {
        return local(Loopback, "loopback");
    }
    if METADATA_V6.contains(&a) {
        return local(Metadata, "metadata");
    }
    if a.is_unicast_link_local() {
        return local(LinkLocal, "link-local");
    }
    if a.is_unique_local() {
        return local(Private, "ula");
    }
    if a.is_multicast() {
        return local(Special, "multicast");
    }
    if in6(a, 0, 96) {
        return embedded(v4_from(bits), "ipv4-compatible");
    }
    if in6(a, 0x2002 << 112, 16) {
        return embedded(v4_from(bits >> 80), "6to4");
    }
    if in6(a, 0x2001_0000 << 96, 32) {
        // Teredo: the client's IPv4 address is the low 32 bits, inverted.
        return embedded(v4_from(!bits), "teredo");
    }
    if !in6(a, 0x2000 << 112, 3) {
        // Outside global unicast: 64:ff9b:1::/48, fec0::/10, 100::/64, ...
        return local(Special, "reserved");
    }
    if in6(a, 0x2001 << 112, 23) {
        return local(Special, "ietf-protocol");
    }
    if in6(a, 0x2001_0db8 << 96, 32) || in6(a, 0x3fff << 112, 20) {
        return local(Special, "documentation");
    }
    AddressClass::Public
}

#[cfg(test)]
mod tests;
