// SPDX-License-Identifier: GPL-3.0-or-later
//! The guard: what a workspace may reach, per destination and per resolved address (R-14).

use std::fmt;
use std::net::SocketAddr;
use std::sync::Arc;

use std::net::IpAddr;

use puddle_types::{BlockReason, CompanySignal, Host, LocalCategory, WorkspaceId};

use crate::{
    AddressClass, CompanySignals, HostAddrs, LocalAccess, LocalAccessSource, NoSignals,
    OwnAddresses, PuddleEndpoints, Target, classify_with_signals,
};

/// What the proxy may do with one resolved address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressVerdict {
    /// Connect if the rules allow the request.
    Allow,
    /// A local address whose toggle is on: connect only after an **exact** allow of the name or
    /// of this address (R-14). After a suffix allow the proxy admits the address if an
    /// exact rule for its IP allows it, and otherwise asks again with `SuffixAllows::Ignore`, so
    /// the request goes pending for the exact name.
    ExactOnly(LocalCategory),
    /// Never connect, whatever the rules say; no pending row.
    Block(BlockReason),
}

/// The destination guard: address classes, the workspace's local toggles and Puddle's own
/// endpoints. Cheap to share (`Arc`); every check reads the workspace's current settings.
///
/// ```
/// use std::sync::Arc;
/// use puddle_netpolicy::{AddressVerdict, LocalAccess, NetPolicy};
/// use puddle_types::{BlockReason, LocalCategory, WorkspaceId};
///
/// let guard = NetPolicy::new(Arc::new(LocalAccess::NONE.with_toggle(LocalCategory::Private, true)));
/// let workspace = WorkspaceId::new("box").unwrap();
/// assert_eq!(guard.check_address(&workspace, "8.8.8.8:443".parse().unwrap()), AddressVerdict::Allow);
/// assert_eq!(
///     guard.check_address(&workspace, "10.0.0.1:443".parse().unwrap()),
///     AddressVerdict::ExactOnly(LocalCategory::Private)
/// );
/// assert_eq!(
///     guard.check_address(&workspace, "127.0.0.1:80".parse().unwrap()),
///     AddressVerdict::Block(BlockReason::LocalToggle(LocalCategory::Loopback))
/// );
/// ```
#[derive(Clone)]
pub struct NetPolicy {
    access: Arc<dyn LocalAccessSource>,
    endpoints: PuddleEndpoints,
    host: Arc<dyn HostAddrs>,
    company: Arc<dyn CompanySignals>,
}

impl fmt::Debug for NetPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NetPolicy")
            .field("endpoints", &self.endpoints)
            .finish_non_exhaustive()
    }
}

impl NetPolicy {
    /// A guard that reads each workspace's toggles from `access`, with an empty endpoint registry
    /// and the host's real addresses ([`OwnAddresses`]).
    #[must_use]
    pub fn new(access: Arc<dyn LocalAccessSource>) -> Self {
        Self {
            access,
            endpoints: PuddleEndpoints::new(),
            host: Arc::new(OwnAddresses),
            company: Arc::new(NoSignals),
        }
    }

    /// Reads the company-network signals from `signals`. Without it the category is empty: no
    /// address is ever in it.
    #[must_use]
    pub fn with_company_signals(mut self, signals: Arc<dyn CompanySignals>) -> Self {
        self.company = signals;
        self
    }

    /// Uses `endpoints` as the registry of Puddle's own listeners.
    #[must_use]
    pub fn with_endpoints(mut self, endpoints: PuddleEndpoints) -> Self {
        self.endpoints = endpoints;
        self
    }

    /// Uses `host` to tell which addresses land on this host.
    #[must_use]
    pub fn with_host_addrs(mut self, host: Arc<dyn HostAddrs>) -> Self {
        self.host = host;
        self
    }

    /// The registry of Puddle's own listeners, to register them into.
    #[must_use]
    pub fn endpoints(&self) -> &PuddleEndpoints {
        &self.endpoints
    }

    /// The class of `addr`: one of Puddle's own endpoints first (whatever its address class),
    /// then the address's class.
    #[must_use]
    pub fn classify(&self, addr: SocketAddr) -> AddressClass {
        self.classify_named(None, addr)
    }

    /// Like [`Self::classify`], for an address reached by `name`: a private address the host's
    /// signals (including those about the name) put in the company network is
    /// [`AddressClass::Company`].
    #[must_use]
    pub fn classify_named(&self, name: Option<&Host>, addr: SocketAddr) -> AddressClass {
        if let Some(kind) = self.endpoints.find(addr, self.host.as_ref()) {
            return AddressClass::PuddleEndpoint(kind);
        }
        classify_with_signals(addr.ip(), name, self.company.as_ref())
    }

    /// The verdict on one address `workspace` would connect to.
    #[must_use]
    pub fn check_address(&self, workspace: &WorkspaceId, addr: SocketAddr) -> AddressVerdict {
        self.check_address_named(workspace, None, addr)
    }

    /// The verdict on one address `workspace` would connect to by `name`.
    #[must_use]
    pub fn check_address_named(
        &self,
        workspace: &WorkspaceId,
        name: Option<&Host>,
        addr: SocketAddr,
    ) -> AddressVerdict {
        self.check_address_with(self.access.local_access(workspace), name, addr)
    }

    /// The verdict on one address reached by `name` under `access`, for a caller with no
    /// workspace (Puddle's own requests) that brings its settings itself.
    #[must_use]
    pub fn check_address_with(
        &self,
        access: LocalAccess,
        name: Option<&Host>,
        addr: SocketAddr,
    ) -> AddressVerdict {
        verdict(self.classify_named(name, addr), access)
    }

    /// The company-network signal that puts `addr` (reached by `name`) in that category, for the
    /// audit; `None` when it is not in it.
    #[must_use]
    pub fn company_signal(&self, name: Option<&Host>, addr: SocketAddr) -> Option<CompanySignal> {
        match self.classify_named(name, addr) {
            AddressClass::Company(signal) => Some(signal),
            AddressClass::Public | AddressClass::Local { .. } | AddressClass::PuddleEndpoint(_) => {
                None
            }
        }
    }

    /// Whether `addr`, reached by `name`, is an address a certificate distribution point may be
    /// fetched from without a rule: in the company network, or a private (RFC 1918 and
    /// alike) address. Loopback, link-local, metadata and special-use addresses, Puddle's own
    /// listeners and public addresses never qualify.
    #[must_use]
    pub fn pki_address(&self, name: Option<&Host>, addr: SocketAddr) -> bool {
        match self.classify_named(name, addr) {
            AddressClass::Company(_) => true,
            AddressClass::Local { category, .. } => category == LocalCategory::Private,
            AddressClass::Public | AddressClass::PuddleEndpoint(_) => false,
        }
    }

    /// The name stage, before the rules are asked: a destination that is blocked by its literal
    /// address or by its name alone (`localhost`, a metadata name) gets its block reason here,
    /// so it never becomes a pending row (R-14). `None`: ask the rules.
    #[must_use]
    pub fn check_target(
        &self,
        workspace: &WorkspaceId,
        target: &Target,
        port: u16,
    ) -> Option<BlockReason> {
        self.check_target_with(self.access.local_access(workspace), target, port)
    }

    /// [`Self::check_target`] under `access`, for a caller with no workspace.
    #[must_use]
    pub fn check_target_with(
        &self,
        access: LocalAccess,
        target: &Target,
        port: u16,
    ) -> Option<BlockReason> {
        if let Host::Ip(ip) = target.host() {
            return match verdict(self.classify(SocketAddr::new(*ip, port)), access) {
                AddressVerdict::Block(reason) => Some(reason),
                AddressVerdict::Allow | AddressVerdict::ExactOnly(_) => None,
            };
        }
        if let Host::Name(name) = target.host()
            && self.endpoints.find_name(name.as_str()).is_some()
        {
            return Some(BlockReason::PuddleEndpoint);
        }
        let category = target.named_category()?;
        // `localhost:<puddle port>`: refuse now rather than after a useless approval.
        let loopback = SocketAddr::from(([127, 0, 0, 1], port));
        if category == LocalCategory::Loopback
            && self.endpoints.find(loopback, self.host.as_ref()).is_some()
        {
            return Some(BlockReason::PuddleEndpoint);
        }
        (!access.is_on(category)).then(|| off_reason(category, access))
    }
}

/// Why a local category is off: its toggle, or settings that could not be read.
fn off_reason(category: LocalCategory, access: LocalAccess) -> BlockReason {
    if access.is_unreadable() {
        BlockReason::SettingsUnreadable
    } else {
        BlockReason::LocalToggle(category)
    }
}

fn verdict(class: AddressClass, access: LocalAccess) -> AddressVerdict {
    match class {
        AddressClass::Public => AddressVerdict::Allow,
        AddressClass::Local { category, .. } if !access.is_on(category) => {
            AddressVerdict::Block(off_reason(category, access))
        }
        AddressClass::Local { .. } if access.wildcards_reach_local() => AddressVerdict::Allow,
        AddressClass::Local { category, .. } => AddressVerdict::ExactOnly(category),
        AddressClass::Company(signal) if !access.is_on(LocalCategory::CompanyNetwork) => {
            AddressVerdict::Block(if access.is_unreadable() {
                BlockReason::SettingsUnreadable
            } else {
                BlockReason::CompanyNetwork(signal)
            })
        }
        AddressClass::Company(_) if access.wildcards_reach_local() => AddressVerdict::Allow,
        AddressClass::Company(_) => AddressVerdict::ExactOnly(LocalCategory::CompanyNetwork),
        // Puddle's own endpoints are never reachable. Listed, so a new class is a compile error here.
        AddressClass::PuddleEndpoint(_) => AddressVerdict::Block(BlockReason::PuddleEndpoint),
    }
}

/// The user-facing explanation for a block (the `403` body), naming what would change it.
///
/// ```
/// use puddle_netpolicy::block_message;
/// use puddle_types::{BlockReason, Host, LocalCategory, WorkspaceId};
///
/// let workspace = WorkspaceId::new("box").unwrap();
/// let host = Host::parse_normalised("10.0.0.1").unwrap();
/// let msg = block_message(&host, &workspace, &[BlockReason::LocalToggle(LocalCategory::Private)]);
/// assert!(msg.contains("turn on 'private' (local/private network) globally or for workspace box"));
/// ```
#[must_use]
pub fn block_message(host: &Host, workspace: &WorkspaceId, reasons: &[BlockReason]) -> String {
    let mut toggles: Vec<LocalCategory> = reasons
        .iter()
        .filter_map(|r| match r {
            BlockReason::LocalToggle(c) => Some(*c),
            BlockReason::SshUnsupported
            | BlockReason::UdpUnsupported
            | BlockReason::PuddleEndpoint
            | BlockReason::LocalAddress
            | BlockReason::SettingsUnreadable
            | BlockReason::CompanyNetwork(_) => None,
        })
        .collect();
    toggles.sort_unstable();
    toggles.dedup();
    if reasons.contains(&BlockReason::SettingsUnreadable) {
        return format!(
            "{host} is a local address, and Puddle could not read its settings, so every local address stays blocked until they can be read. The Settings page says what is wrong; once the settings are readable again, the toggles apply as set"
        );
    }
    if toggles.is_empty() {
        if let Some(signal) = reasons.iter().find_map(|r| match r {
            BlockReason::CompanyNetwork(signal) => Some(*signal),
            BlockReason::SshUnsupported
            | BlockReason::UdpUnsupported
            | BlockReason::PuddleEndpoint
            | BlockReason::LocalAddress
            | BlockReason::LocalToggle(_)
            | BlockReason::SettingsUnreadable => None,
        }) {
            return company_network_message(host, None, Some(signal));
        }
        return match reasons.first() {
            Some(BlockReason::PuddleEndpoint) => format!(
                "{host} is one of Puddle's own endpoints; workspaces never reach them, and no rule or toggle changes this"
            ),
            Some(BlockReason::SshUnsupported) => ssh_unsupported_message(host),
            Some(BlockReason::UdpUnsupported) => format!(
                "UDP to {host} is not supported yet; only TCP and DNS leave a workspace, and no rule or setting changes this"
            ),
            _ => format!("{host} is a local address Puddle does not connect to"),
        };
    }
    let what = toggles
        .iter()
        .map(|c| c.describe())
        .collect::<Vec<_>>()
        .join(" or ");
    let switch = toggles
        .iter()
        .map(|c| format!("'{}' ({})", c.key(), c.describe()))
        .collect::<Vec<_>>()
        .join(" or ");
    format!(
        "{host} is {what}, and that toggle is off. To allow it, turn on {switch} globally or for workspace {workspace}; it then still needs an allow rule or an approval. Approving it alone does not change this"
    )
}

/// The refusal text for a destination in the company network whose toggle is off: what was
/// found (the host, the address it resolved to when it is not the host itself, the signal when
/// known) and the switch. It never says that the switch allows the destination: it only makes it
/// possible to allow it.
///
/// ```
/// use puddle_netpolicy::company_network_message;
/// use puddle_types::{CompanySignal, Host};
///
/// let host = Host::parse_normalised("nexus.corp.example").unwrap();
/// let msg = company_network_message(&host, Some("100.64.3.7".parse().unwrap()), Some(CompanySignal::PrivateAccessCgnat));
/// assert_eq!(
///     msg,
///     "nexus.corp.example (100.64.3.7) is on your company network (reached through a private-access client). Turn on 'Company network' for this workspace to be able to allow it"
/// );
/// ```
#[must_use]
pub fn company_network_message(
    host: &Host,
    ip: Option<IpAddr>,
    signal: Option<CompanySignal>,
) -> String {
    let address = match (host, ip) {
        (Host::Name(_), Some(ip)) => format!(" ({ip})"),
        _ => String::new(),
    };
    let found = signal.map_or_else(String::new, |s| {
        format!(" (reached through {})", s.describe())
    });
    format!(
        "{host}{address} is on your company network{found}. Turn on 'Company network' for this workspace to be able to allow it"
    )
}

/// The Git host name that Azure DevOps serves SSH on, at port 22 only.
pub const AZURE_DEVOPS_SSH_HOST: &str = "ssh.dev.azure.com";

/// The refusal text for SSH to `ssh.dev.azure.com:22` when the host's own probe of that port
/// failed: the company network blocks it, and an HTTPS remote needs no key.
///
/// ```
/// use puddle_netpolicy::azure_devops_ssh_message;
/// assert_eq!(
///     azure_devops_ssh_message(),
///     "your company network blocks SSH to ssh.dev.azure.com:22; HTTPS remotes work and need no key: Puddle adds your credentials"
/// );
/// ```
#[must_use]
pub fn azure_devops_ssh_message() -> String {
    format!(
        "your company network blocks SSH to {AZURE_DEVOPS_SSH_HOST}:22; HTTPS remotes work and need no key: Puddle adds your credentials"
    )
}

/// The HTTPS form of a repository on a well-known Git host that people reach over SSH, with the
/// parts that differ per repository in capitals; `None` for any other host.
///
/// ```
/// use puddle_netpolicy::git_https_remote;
/// use puddle_types::Host;
///
/// let host = Host::parse_normalised("ssh.github.com").unwrap();
/// assert_eq!(git_https_remote(&host), Some("https://github.com/OWNER/REPO.git"));
/// let other = Host::parse_normalised("git.example.com").unwrap();
/// assert_eq!(git_https_remote(&other), None);
/// ```
#[must_use]
pub fn git_https_remote(host: &Host) -> Option<&'static str> {
    let Host::Name(name) = host else {
        return None;
    };
    match name.as_str() {
        "github.com" | "ssh.github.com" => Some("https://github.com/OWNER/REPO.git"),
        "gitlab.com" => Some("https://gitlab.com/GROUP/PROJECT.git"),
        "bitbucket.org" => Some("https://bitbucket.org/WORKSPACE/REPO.git"),
        "ssh.dev.azure.com" | "vs-ssh.visualstudio.com" => {
            Some("https://dev.azure.com/ORGANIZATION/PROJECT/_git/REPO")
        }
        _ => None,
    }
}

/// The refusal text for SSH to a Git host's name that Puddle does not serve: it names the HTTPS
/// form of the remote when it is a built-in one (a Git credential's own host gets the plain line).
fn ssh_unsupported_message(host: &Host) -> String {
    match git_https_remote(host) {
        Some(https) => format!("SSH is not supported yet, use HTTPS: the remote becomes {https}"),
        None => "SSH is not supported yet; no rule or setting allows it".to_owned(),
    }
}

#[cfg(test)]
mod tests;
