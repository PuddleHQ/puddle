// SPDX-License-Identifier: AGPL-3.0-or-later
//! The host's readers for the company-network category: what the machine's network says about
//! which private addresses are the company's (`puddle_netpolicy::CompanySignals`).
//!
//! Read-only, and never in the way of a connection: the network is read at most every few
//! seconds into a snapshot, and the proxy script is asked in the background. A signal that cannot
//! be read answers "no" (the address stays local/private), except the LAN reading, whose
//! unreadable answer also keeps an address out of the category (`LanReading::Unread`).
//!
//! | Signal | Read from |
//! |---|---|
//! | proxy script says `DIRECT` | the system's PAC or WPAD ([`pac`]), against a reference URL it proxies |
//! | DNS suffix or namespace | connection suffixes of adapters that are up, primary suffix, suffix search list, name-resolution policy table |
//! | private-access client | an adapter that is up and carries a known client's name (Zscaler, Netskope, WARP, Tailscale) |
//! | VPN route | the best IPv4 route for the address leaves by a VPN adapter |
//! | physical LAN | the subnets of the up adapters that are neither VPN, private-access nor loopback |
//!
//! Windows is the only platform with readers. On Linux the category stays empty (the open
//! question of the requirements, answered "no signals" until a user asks for `resolvectl`); on
//! macOS it is empty as well, because the readers would need a macOS machine to test.

mod pac;

use std::net::{IpAddr, Ipv4Addr};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use puddle_netpolicy::{CompanyFound, CompanySignals, LanReading, NoSignals, StaticSignals};
use puddle_upstream::{Adapter, AdapterKind, NetworkOs, NetworkSnapshot, OsProxy};

pub use pac::{PacReader, PacTiming};

/// How long a read of the network is reused.
const SNAPSHOT_TTL: Duration = Duration::from_secs(10);

/// Descriptions and names of private-access clients (Zscaler, Netskope, Cloudflare WARP,
/// Tailscale, Microsoft Entra Private Access), lower case.
const PRIVATE_ACCESS_CLIENTS: [&str; 6] = [
    "zscaler",
    "netskope",
    "warp",
    "tailscale",
    "private access",
    "global secure access",
];
/// Descriptions and names of VPN clients and drivers, lower case.
const VPN_ADAPTERS: [&str; 12] = [
    "vpn",
    "tap-windows",
    "openvpn",
    "wireguard",
    "wintun",
    "anyconnect",
    "globalprotect",
    "pangp",
    "forticlient",
    "fortinet",
    "sonicwall",
    "juniper",
];

/// The company-network signals of the machine the host runs on.
pub struct HostSignals {
    network: Arc<dyn NetworkOs>,
    pac: Option<PacReader>,
    ttl: Duration,
    cached: Mutex<Option<(Instant, Arc<Reading>)>>,
}

impl std::fmt::Debug for HostSignals {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostSignals").finish_non_exhaustive()
    }
}

impl HostSignals {
    /// The machine's own signals: the operating system's network and proxy script. On a platform
    /// without readers the category is empty.
    #[must_use]
    pub fn system() -> Arc<dyn CompanySignals> {
        if cfg!(windows) {
            Arc::new(Self::new(
                puddle_upstream::system_network(),
                Some(puddle_upstream::system_os()),
            ))
        } else {
            Arc::new(NoSignals)
        }
    }

    /// Signals over `network` and, for the proxy script, `proxy`.
    #[must_use]
    pub fn new(network: Arc<dyn NetworkOs>, proxy: Option<Arc<dyn OsProxy>>) -> Self {
        Self {
            network,
            pac: proxy.map(|os| PacReader::new(os, PacTiming::default())),
            ttl: SNAPSHOT_TTL,
            cached: Mutex::new(None),
        }
    }

    /// Reuses a read of the network for `ttl` (the default is a few seconds).
    #[must_use]
    pub fn with_snapshot_ttl(mut self, ttl: Duration) -> Self {
        self.ttl = ttl;
        self
    }

    /// Takes the proxy-script reader's timing and the way it runs its background questions from
    /// `pac` (tests run them in place).
    #[must_use]
    pub fn with_pac(mut self, pac: PacReader) -> Self {
        self.pac = Some(pac);
        self
    }

    fn reading(&self) -> Arc<Reading> {
        let mut cached = self.cached.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((read_at, reading)) = cached.as_ref()
            && read_at.elapsed() < self.ttl
        {
            return reading.clone();
        }
        let reading = Arc::new(match self.network.read() {
            Ok(snapshot) => Reading::of(&snapshot),
            Err(err) => {
                tracing::debug!(error = %err, "the company-network signals could not read the network");
                Reading::Unread
            }
        });
        *cached = Some((Instant::now(), reading.clone()));
        reading
    }
}

impl CompanySignals for HostSignals {
    fn pac_direct(&self, name: &str) -> bool {
        self.pac.as_ref().is_some_and(|pac| pac.direct(name))
    }

    fn in_dns_namespace(&self, name: &str) -> bool {
        match &*self.reading() {
            Reading::Read(read) => read.fixed.in_dns_namespace(name),
            Reading::Unread => false,
        }
    }

    fn private_access_client(&self) -> bool {
        match &*self.reading() {
            Reading::Read(read) => read.fixed.private_access_client(),
            Reading::Unread => false,
        }
    }

    fn vpn_routed(&self, ip: IpAddr) -> bool {
        match (&*self.reading(), ip) {
            (Reading::Read(read), IpAddr::V4(v4)) => read.vpn_routed(v4),
            _ => false,
        }
    }

    fn physical_lan(&self, ip: IpAddr) -> LanReading {
        match &*self.reading() {
            Reading::Read(read) => read.fixed.physical_lan(ip),
            Reading::Unread => LanReading::Unread,
        }
    }

    fn found(&self) -> CompanyFound {
        match &*self.reading() {
            Reading::Read(read) => read.fixed.found(),
            Reading::Unread => CompanyFound::default(),
        }
    }
}

/// One read of the network, boiled down to what the signals answer from.
enum Reading {
    Read(Box<Read>),
    Unread,
}

struct Read {
    /// The signals that are a plain set: suffixes, the client flag and the LAN subnets.
    fixed: StaticSignals,
    /// The IPv4 routes of adapters that are up, with whether the adapter is a VPN.
    routes: Vec<ViaRoute>,
}

struct ViaRoute {
    net: Ipv4Addr,
    bits: u8,
    metric: u64,
    vpn: bool,
}

/// What an adapter is for the category.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    /// A physical LAN link.
    Lan,
    /// A VPN link.
    Vpn,
    /// A known private-access client's link (also a VPN link for the routes).
    PrivateAccess,
    /// Loopback and the like: not a network the machine is on.
    Ignored,
}

fn role(adapter: &Adapter) -> Role {
    if adapter.kind == AdapterKind::Loopback {
        return Role::Ignored;
    }
    let text = format!("{} {}", adapter.name, adapter.description).to_ascii_lowercase();
    let mentions = |words: &[&str]| words.iter().any(|word| text.contains(word));
    if mentions(&PRIVATE_ACCESS_CLIENTS) {
        Role::PrivateAccess
    } else if matches!(
        adapter.kind,
        AdapterKind::PointToPoint | AdapterKind::Tunnel
    ) || mentions(&VPN_ADAPTERS)
    {
        Role::Vpn
    } else {
        Role::Lan
    }
}

/// A namespace as a DNS suffix, or `None` when it is not one (a wildcard of everything, text
/// that is no name).
fn suffix_of(namespace: &str) -> Option<String> {
    let trimmed = namespace.trim().trim_start_matches("*.");
    let valid = trimmed
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'));
    (valid && !trimmed.trim_matches('.').is_empty()).then(|| trimmed.to_owned())
}

impl Reading {
    fn of(snapshot: &NetworkSnapshot) -> Self {
        let mut fixed = StaticSignals::new();
        let mut roles = Vec::new();
        for adapter in snapshot.adapters.iter().filter(|adapter| adapter.up) {
            let role = role(adapter);
            roles.push((adapter, role));
            if role == Role::Ignored {
                continue;
            }
            if let Some(suffix) = adapter.dns_suffix.as_deref().and_then(suffix_of) {
                fixed = fixed.with_dns_suffix(&suffix);
            }
            match role {
                Role::Lan => {
                    for &(net, bits) in &adapter.subnets {
                        fixed = fixed.with_lan_subnet(net, bits);
                    }
                }
                Role::PrivateAccess => fixed = fixed.with_private_access_client(),
                Role::Vpn | Role::Ignored => {}
            }
        }
        for suffix in snapshot.dns_namespaces.iter().filter_map(|n| suffix_of(n)) {
            fixed = fixed.with_dns_suffix(&suffix);
        }
        let routes = snapshot
            .routes
            .iter()
            .filter_map(|route| {
                let IpAddr::V4(net) = route.net else {
                    return None;
                };
                let (adapter, role) = roles
                    .iter()
                    .find(|(adapter, _)| adapter.index == route.interface)?;
                Some(ViaRoute {
                    net,
                    bits: route.bits,
                    metric: u64::from(route.metric) + u64::from(adapter.metric),
                    vpn: matches!(role, Role::Vpn | Role::PrivateAccess),
                })
            })
            .collect();
        Self::Read(Box::new(Read { fixed, routes }))
    }
}

impl Read {
    /// Whether the route Windows would pick for `ip` (the longest prefix, then the lowest metric)
    /// leaves by a VPN adapter.
    fn vpn_routed(&self, ip: Ipv4Addr) -> bool {
        self.routes
            .iter()
            .filter(|route| route.bits <= 32 && covers(route.net, route.bits, ip))
            .max_by_key(|route| (route.bits, std::cmp::Reverse(route.metric)))
            .is_some_and(|route| route.vpn)
    }
}

fn covers(net: Ipv4Addr, bits: u8, ip: Ipv4Addr) -> bool {
    let mask = u32::MAX.checked_shl(32 - u32::from(bits)).unwrap_or(0);
    net.to_bits() & mask == ip.to_bits() & mask
}

#[cfg(test)]
mod tests;
