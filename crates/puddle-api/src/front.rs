// SPDX-License-Identifier: GPL-3.0-or-later
//! The addresses of a reverse proxy in front of Puddle that runs on another machine.
//!
//! The guard blocks the front's names, but a guest can name the same machine in other ways: its
//! address, or any name whose record points at it. So the operator's front is also blocked by
//! address: the proxies Puddle admits, a public host written as an address, and what each public
//! name and the workspace base domain resolve to. The addresses go in the endpoint registry on
//! the front's ports, where the proxy checks every address a guest would connect to.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use puddle_netpolicy::{AddressClass, EndpointKind, PuddleEndpoints, Registration, classify_ip};

use crate::remote::{Remote, RemoteServing};

/// How long Puddle waits for one name to resolve.
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(5);

/// How often the names are resolved again: a record can move to another machine.
pub(crate) const REFRESH_EVERY: Duration = Duration::from_secs(60);

/// What the front's names resolve to. A seam so a test does not need DNS.
pub trait NameResolver: Send + Sync {
    /// The addresses `name` has now.
    fn resolve<'a>(
        &'a self,
        name: &'a str,
    ) -> Pin<Box<dyn Future<Output = io::Result<Vec<IpAddr>>> + Send + 'a>>;
}

/// The system's resolver.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemResolver;

impl NameResolver for SystemResolver {
    fn resolve<'a>(
        &'a self,
        name: &'a str,
    ) -> Pin<Box<dyn Future<Output = io::Result<Vec<IpAddr>>> + Send + 'a>> {
        Box::pin(async move {
            let found = tokio::net::lookup_host((name, 0)).await?;
            Ok(found.map(|addr| addr.ip()).collect())
        })
    }
}

/// The names to resolve and the registrations they hold.
pub(crate) struct FrontAddresses {
    endpoints: PuddleEndpoints,
    resolver: std::sync::Arc<dyn NameResolver>,
    ports: Vec<u16>,
    names: Vec<String>,
    /// Addresses the operator wrote down: held for as long as the API serves.
    _fixed: Vec<Registration>,
    /// What each name resolved to the last time it did.
    resolved: Mutex<HashMap<String, Vec<Registration>>>,
}

impl FrontAddresses {
    /// Registers the addresses that need no lookup and prepares the lookups.
    pub(crate) fn new(
        remote: &Remote,
        endpoints: &PuddleEndpoints,
        resolver: std::sync::Arc<dyn NameResolver>,
    ) -> Self {
        let serving = remote.serving();
        let ports = front_ports(serving);
        let mut fixed_ips: HashSet<IpAddr> = remote
            .network_listener()
            .map(|n| n.endpoint().allowed_proxies().iter().copied().collect())
            .unwrap_or_default();
        let mut names = Vec::new();
        for host in serving.public_hosts() {
            let bare = host.host().trim_start_matches('[').trim_end_matches(']');
            match bare.parse::<IpAddr>() {
                Ok(ip) => {
                    fixed_ips.insert(ip);
                }
                Err(_) => names.push(host.host().to_owned()),
            }
        }
        names.push(serving.workspace_names().base_domain().to_owned());
        names.sort_unstable();
        names.dedup();
        let fixed = fixed_ips
            .into_iter()
            .flat_map(|ip| register_ip(endpoints, ip, &ports))
            .collect();
        Self {
            endpoints: endpoints.clone(),
            resolver,
            ports,
            names,
            _fixed: fixed,
            resolved: Mutex::default(),
        }
    }

    /// Resolves every name again. A name that does not resolve keeps what it had, so a DNS
    /// outage never opens the front to guests.
    pub(crate) async fn refresh(&self) {
        let lookups = self.names.iter().map(|name| async move {
            let found = tokio::time::timeout(LOOKUP_TIMEOUT, self.resolver.resolve(name)).await;
            (name, found)
        });
        for (name, found) in futures_util::future::join_all(lookups).await {
            let ips = match found {
                Ok(Ok(ips)) => ips,
                Ok(Err(error)) => {
                    tracing::warn!(%name, %error, "the front's name did not resolve; keeping its last addresses");
                    continue;
                }
                Err(_) => {
                    tracing::warn!(%name, "resolving the front's name timed out; keeping its last addresses");
                    continue;
                }
            };
            // Registered before the old ones go, so no moment is left open.
            let fresh: Vec<Registration> = unique(ips)
                .into_iter()
                .filter(|ip| !matches!(classify_ip(*ip), AddressClass::Public))
                .flat_map(|ip| register_ip(&self.endpoints, ip, &self.ports))
                .collect();
            self.resolved
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(name.clone(), fresh);
        }
    }
}

/// Every port a public host listens on, once.
fn front_ports(serving: &RemoteServing) -> Vec<u16> {
    let mut ports: Vec<u16> = serving
        .public_hosts()
        .iter()
        .map(crate::remote::PublicHost::port)
        .collect();
    ports.sort_unstable();
    ports.dedup();
    ports
}

fn unique(ips: Vec<IpAddr>) -> Vec<IpAddr> {
    let mut seen = HashSet::new();
    ips.into_iter()
        .map(|ip| match ip {
            IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
            IpAddr::V4(_) => ip,
        })
        .filter(|ip| seen.insert(*ip))
        .collect()
}

fn register_ip(endpoints: &PuddleEndpoints, ip: IpAddr, ports: &[u16]) -> Vec<Registration> {
    ports
        .iter()
        .map(|port| endpoints.register(SocketAddr::new(ip, *port), EndpointKind::ExternalFront))
        .collect()
}

#[cfg(test)]
mod tests;
