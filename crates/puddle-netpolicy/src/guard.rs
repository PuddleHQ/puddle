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
#[non_exhaustive]
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
        verdict(
            self.classify_named(name, addr),
            self.access.local_access(workspace),
        )
    }

    /// The company-network signal that puts `addr` (reached by `name`) in that category, for the
    /// audit; `None` when it is not in it.
    #[must_use]
    pub fn company_signal(&self, name: Option<&Host>, addr: SocketAddr) -> Option<CompanySignal> {
        match self.classify_named(name, addr) {
            AddressClass::Company(signal) => Some(signal),
            _ => None,
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
        let access = self.access.local_access(workspace);
        if let Host::Ip(ip) = target.host() {
            return match verdict(self.classify(SocketAddr::new(*ip, port)), access) {
                AddressVerdict::Block(reason) => Some(reason),
                _ => None,
            };
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
        // Puddle's own endpoints, and any class this version doesn't know: blocked.
        _ => AddressVerdict::Block(BlockReason::PuddleEndpoint),
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
            _ => None,
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
            _ => None,
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

/// The refusal text for SSH: every SSH connection gets it, a Git host's also names the HTTPS form
/// of the remote.
fn ssh_unsupported_message(host: &Host) -> String {
    match git_https_remote(host) {
        Some(https) => format!("SSH is not supported yet, use HTTPS: the remote becomes {https}"),
        None => "SSH is not supported yet; no rule or setting allows it".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use std::net::IpAddr;

    use super::*;
    use crate::{EndpointKind, Registration, normalise_host};

    fn workspace() -> WorkspaceId {
        WorkspaceId::new("box").unwrap()
    }

    fn sa(s: &str) -> SocketAddr {
        s.parse().unwrap()
    }

    struct Lan;
    impl HostAddrs for Lan {
        fn is_own(&self, ip: IpAddr) -> bool {
            ip == "192.168.1.10".parse::<IpAddr>().unwrap()
        }
    }

    fn guard(access: LocalAccess) -> (NetPolicy, Registration) {
        let g = NetPolicy::new(Arc::new(access)).with_host_addrs(Arc::new(Lan));
        let api = g
            .endpoints()
            .register(sa("127.0.0.1:7070"), EndpointKind::Api);
        (g, api)
    }

    fn all_on() -> LocalAccess {
        LocalCategory::ALL
            .into_iter()
            .fold(LocalAccess::NONE, |a, c| a.with_toggle(c, true))
    }

    #[test]
    fn toggle_off_blocks_naming_the_category() {
        let (g, _api) = guard(LocalAccess::NONE);
        for (addr, c) in [
            ("127.0.0.1:80", LocalCategory::Loopback),
            ("10.1.2.3:80", LocalCategory::Private),
            ("169.254.1.1:80", LocalCategory::LinkLocal),
            ("169.254.169.254:80", LocalCategory::Metadata),
            ("224.0.0.1:80", LocalCategory::Special),
        ] {
            assert_eq!(
                g.check_address(&workspace(), sa(addr)),
                AddressVerdict::Block(BlockReason::LocalToggle(c)),
                "{addr}"
            );
        }
        assert_eq!(
            g.check_address(&workspace(), sa("8.8.8.8:80")),
            AddressVerdict::Allow
        );
    }

    #[test]
    fn toggle_on_permits_exact_allows_only_unless_wildcards_reach_local() {
        let (g, _api) = guard(all_on());
        assert_eq!(
            g.check_address(&workspace(), sa("10.1.2.3:80")),
            AddressVerdict::ExactOnly(LocalCategory::Private)
        );
        let (g, _api) = guard(all_on().with_wildcards_reach_local(true));
        assert_eq!(
            g.check_address(&workspace(), sa("10.1.2.3:80")),
            AddressVerdict::Allow
        );
        // The wildcard setting never replaces a toggle.
        let (g, _api) = guard(LocalAccess::NONE.with_wildcards_reach_local(true));
        assert_eq!(
            g.check_address(&workspace(), sa("10.1.2.3:80")),
            AddressVerdict::Block(BlockReason::LocalToggle(LocalCategory::Private))
        );
    }

    #[test]
    fn puddle_endpoints_are_their_own_class_and_ignore_every_toggle() {
        let (g, _api) = guard(all_on().with_wildcards_reach_local(true));
        for addr in [
            "127.0.0.1:7070",
            "[::1]:7070",
            "192.168.1.10:7070",
            "0.0.0.0:7070",
        ] {
            assert_eq!(
                g.classify(sa(addr)),
                AddressClass::PuddleEndpoint(EndpointKind::Api),
                "{addr}"
            );
            assert_eq!(
                g.check_address(&workspace(), sa(addr)),
                AddressVerdict::Block(BlockReason::PuddleEndpoint),
                "{addr}"
            );
        }
        assert_eq!(
            g.check_address(&workspace(), sa("127.0.0.1:7071")),
            AddressVerdict::Allow
        );
        assert_eq!(
            g.check_address(&workspace(), sa("192.168.1.11:7070")),
            AddressVerdict::Allow
        );
    }

    #[test]
    fn the_name_stage_blocks_literals_and_category_names_before_the_rules() {
        let target = |h: &str| normalise_host(h).unwrap();
        let (off, _a) = guard(LocalAccess::NONE);
        let check = |g: &NetPolicy, h: &str, port| g.check_target(&workspace(), &target(h), port);
        assert_eq!(
            check(&off, "10.0.0.1", 80),
            Some(BlockReason::LocalToggle(LocalCategory::Private))
        );
        assert_eq!(
            check(&off, "localhost", 80),
            Some(BlockReason::LocalToggle(LocalCategory::Loopback))
        );
        assert_eq!(
            check(&off, "metadata.google.internal", 80),
            Some(BlockReason::LocalToggle(LocalCategory::Metadata))
        );
        assert_eq!(check(&off, "example.com", 80), None);
        assert_eq!(check(&off, "8.8.8.8", 80), None);
        let (on, _b) = guard(all_on());
        // Toggle on: the rules decide (an exact allow or an approval).
        assert_eq!(check(&on, "10.0.0.1", 80), None);
        assert_eq!(check(&on, "localhost", 80), None);
        assert_eq!(check(&on, "metadata", 80), None);
        // Puddle's own endpoints, by literal or by `localhost`, whatever the toggles.
        assert_eq!(
            check(&on, "127.0.0.1", 7070),
            Some(BlockReason::PuddleEndpoint)
        );
        assert_eq!(
            check(&on, "localhost", 7070),
            Some(BlockReason::PuddleEndpoint)
        );
        assert_eq!(
            check(&on, "foo.localhost", 7070),
            Some(BlockReason::PuddleEndpoint)
        );
        assert_eq!(check(&on, "metadata", 7070), None);
        assert_eq!(
            check(&off, "localhost", 7070),
            Some(BlockReason::PuddleEndpoint)
        );
    }

    #[test]
    fn toggles_are_read_per_workspace_per_call() {
        let per = |s: &WorkspaceId| {
            LocalAccess::NONE.with_toggle(LocalCategory::Loopback, s.as_str() == "dev")
        };
        let g = NetPolicy::new(Arc::new(per));
        let dev = WorkspaceId::new("dev").unwrap();
        assert_eq!(
            g.check_address(&dev, sa("127.0.0.1:3000")),
            AddressVerdict::ExactOnly(LocalCategory::Loopback)
        );
        assert_eq!(
            g.check_address(&workspace(), sa("127.0.0.1:3000")),
            AddressVerdict::Block(BlockReason::LocalToggle(LocalCategory::Loopback))
        );
        assert!(format!("{g:?}").starts_with("NetPolicy"));
    }

    #[test]
    fn unreadable_settings_block_local_destinations_with_their_own_reason() {
        let g = NetPolicy::new(Arc::new(LocalAccess::UNREADABLE)).with_host_addrs(Arc::new(Lan));
        let reason = Some(BlockReason::SettingsUnreadable);
        for addr in ["127.0.0.1:3000", "10.0.0.5:80", "169.254.169.254:80"] {
            assert_eq!(
                g.check_address(&workspace(), sa(addr)),
                AddressVerdict::Block(BlockReason::SettingsUnreadable),
                "{addr}"
            );
        }
        let ip = |s: &str| normalise_host(s).unwrap();
        assert_eq!(g.check_target(&workspace(), &ip("10.0.0.5"), 80), reason);
        assert_eq!(g.check_target(&workspace(), &ip("localhost"), 80), reason);
        assert_eq!(g.check_target(&workspace(), &ip("127.0.0.1"), 80), reason);
        // Public addresses and Puddle's own endpoints are unaffected.
        assert_eq!(
            g.check_address(&workspace(), sa("93.184.216.34:443")),
            AddressVerdict::Allow
        );
        assert_eq!(g.check_target(&workspace(), &ip("example.com"), 443), None);
    }

    #[test]
    fn the_unreadable_settings_message_says_so() {
        let host = Host::parse_normalised("nas.example").unwrap();
        let msg = block_message(
            &host,
            &workspace(),
            &[BlockReason::PuddleEndpoint, BlockReason::SettingsUnreadable],
        );
        assert!(msg.contains("could not read its settings"), "{msg}");
        assert!(msg.contains("Settings page"), "{msg}");
        assert!(!msg.contains("toggle is off"), "{msg}");
    }

    #[test]
    fn block_messages_name_what_would_change_them() {
        let host = Host::parse_normalised("nas.example").unwrap();
        let one = block_message(
            &host,
            &workspace(),
            &[BlockReason::LocalToggle(LocalCategory::Private)],
        );
        assert_eq!(
            one,
            "nas.example is local/private network, and that toggle is off. To allow it, turn on 'private' (local/private network) globally or for workspace box; it then still needs an allow rule or an approval. Approving it alone does not change this"
        );
        let two = block_message(
            &host,
            &workspace(),
            &[
                BlockReason::LocalToggle(LocalCategory::Private),
                BlockReason::PuddleEndpoint,
                BlockReason::LocalToggle(LocalCategory::Loopback),
                BlockReason::LocalToggle(LocalCategory::Private),
            ],
        );
        assert!(
            two.contains("turn on 'loopback' (host loopback) or 'private' (local/private network)"),
            "{two}"
        );
        assert!(
            block_message(&host, &workspace(), &[BlockReason::PuddleEndpoint])
                .contains("Puddle's own endpoints")
        );
        assert_eq!(
            block_message(&host, &workspace(), &[BlockReason::SshUnsupported]),
            "SSH is not supported yet; no rule or setting allows it"
        );
        assert_eq!(
            block_message(&host, &workspace(), &[BlockReason::UdpUnsupported]),
            format!(
                "UDP to {host} is not supported yet; only TCP and DNS leave a workspace, and no rule or setting changes this"
            )
        );
        assert!(
            block_message(&host, &workspace(), &[BlockReason::LocalAddress])
                .contains("local address")
        );
        assert!(block_message(&host, &workspace(), &[]).contains("local address"));
    }

    #[test]
    fn ssh_to_a_git_host_names_the_https_form_of_the_remote_and_any_other_host_does_not() {
        for (host, https) in [
            ("github.com", "https://github.com/OWNER/REPO.git"),
            ("ssh.github.com", "https://github.com/OWNER/REPO.git"),
            ("gitlab.com", "https://gitlab.com/GROUP/PROJECT.git"),
            ("bitbucket.org", "https://bitbucket.org/WORKSPACE/REPO.git"),
            (
                "ssh.dev.azure.com",
                "https://dev.azure.com/ORGANIZATION/PROJECT/_git/REPO",
            ),
            (
                "vs-ssh.visualstudio.com",
                "https://dev.azure.com/ORGANIZATION/PROJECT/_git/REPO",
            ),
        ] {
            let host = Host::parse_normalised(host).unwrap();
            assert_eq!(git_https_remote(&host), Some(https));
            assert_eq!(
                block_message(&host, &workspace(), &[BlockReason::SshUnsupported]),
                format!("SSH is not supported yet, use HTTPS: the remote becomes {https}")
            );
        }
        for other in [
            "git.example.com",
            "gitlab.com.evil.example",
            "notgithub.com",
            "dev.azure.com",
            "203.0.113.7",
            "2001:db8::1",
        ] {
            let host = Host::parse_normalised(other).unwrap();
            assert_eq!(git_https_remote(&host), None, "{other}");
            assert_eq!(
                block_message(&host, &workspace(), &[BlockReason::SshUnsupported]),
                "SSH is not supported yet; no rule or setting allows it",
                "{other}"
            );
        }
    }

    #[test]
    fn classify_recognises_the_endpoints_of_a_registry_it_was_given() {
        let registry = PuddleEndpoints::new();
        let _api = registry.register(sa("127.0.0.1:7071"), EndpointKind::Api);
        let g = NetPolicy::new(Arc::new(LocalAccess::NONE)).with_endpoints(registry);
        assert_eq!(
            g.classify(sa("127.0.0.1:7071")),
            AddressClass::PuddleEndpoint(EndpointKind::Api)
        );
        assert_eq!(g.endpoints().list().len(), 1);
    }
    // The company-network category (the host's signals decide; the toggle decides what follows).

    use crate::StaticSignals;

    fn company_guard(access: LocalAccess) -> NetPolicy {
        let signals = StaticSignals::new()
            .with_dns_suffix("corp.example")
            .with_private_access_client()
            .with_vpn_route("10.8.0.0".parse().unwrap(), 16)
            .with_lan_subnet("192.168.1.0".parse().unwrap(), 24);
        NetPolicy::new(Arc::new(access))
            .with_host_addrs(Arc::new(Lan))
            .with_company_signals(Arc::new(signals))
    }

    fn named(s: &str) -> Host {
        Host::parse_normalised(s).unwrap()
    }

    #[test]
    fn company_toggle_is_off_by_default_and_blocks_with_the_signal() {
        let g = company_guard(all_on_but_company());
        let w = workspace();
        let host = named("nexus.corp.example");
        assert_eq!(
            g.check_address_named(&w, Some(&host), sa("10.1.2.3:443")),
            AddressVerdict::Block(BlockReason::CompanyNetwork(CompanySignal::DnsSuffix))
        );
        assert_eq!(
            g.check_address_named(&w, None, sa("100.64.3.7:443")),
            AddressVerdict::Block(BlockReason::CompanyNetwork(
                CompanySignal::PrivateAccessCgnat
            ))
        );
        assert_eq!(
            g.check_address_named(&w, None, sa("10.8.1.1:443")),
            AddressVerdict::Block(BlockReason::CompanyNetwork(CompanySignal::VpnRoute))
        );
    }

    fn all_on_but_company() -> LocalAccess {
        all_on().with_toggle(LocalCategory::CompanyNetwork, false)
    }

    #[test]
    fn company_toggle_on_permits_exact_allows_only_and_never_the_private_toggle() {
        let w = workspace();
        // Company on, private off: the company address is reachable, a plain private one is not.
        let on = LocalAccess::NONE.with_toggle(LocalCategory::CompanyNetwork, true);
        let g = company_guard(on);
        assert_eq!(
            g.check_address(&w, sa("100.64.3.7:443")),
            AddressVerdict::ExactOnly(LocalCategory::CompanyNetwork)
        );
        assert_eq!(
            g.check_address(&w, sa("172.16.0.9:443")),
            AddressVerdict::Block(BlockReason::LocalToggle(LocalCategory::Private))
        );
        // Wildcards reach it only when that setting is on.
        let g = company_guard(on.with_wildcards_reach_local(true));
        assert_eq!(
            g.check_address(&w, sa("100.64.3.7:443")),
            AddressVerdict::Allow
        );
        // Company off, private on: the company address still needs the company toggle.
        let g = company_guard(LocalAccess::NONE.with_toggle(LocalCategory::Private, true));
        assert!(matches!(
            g.check_address(&w, sa("100.64.3.7:443")),
            AddressVerdict::Block(BlockReason::CompanyNetwork(_))
        ));
    }

    #[test]
    fn what_the_company_toggle_never_reaches() {
        let g = company_guard(LocalAccess::NONE.with_toggle(LocalCategory::CompanyNetwork, true));
        let w = workspace();
        let host = named("nexus.corp.example");
        for (addr, category) in [
            ("127.0.0.1:80", LocalCategory::Loopback),
            ("169.254.1.1:80", LocalCategory::LinkLocal),
            ("169.254.169.254:80", LocalCategory::Metadata),
            ("224.0.0.1:80", LocalCategory::Special),
        ] {
            assert_eq!(
                g.check_address_named(&w, Some(&host), sa(addr)),
                AddressVerdict::Block(BlockReason::LocalToggle(category)),
                "{addr}"
            );
        }
        // A public address stays public whatever its name says.
        assert_eq!(
            g.check_address_named(&w, Some(&host), sa("8.8.8.8:443")),
            AddressVerdict::Allow
        );
        // The physical LAN's own subnet stays local/private even under a company suffix.
        assert_eq!(
            g.check_address_named(&w, Some(&host), sa("192.168.1.40:443")),
            AddressVerdict::Block(BlockReason::LocalToggle(LocalCategory::Private))
        );
        // Puddle's own listener.
        let (g2, _api) = guard(LocalAccess::NONE);
        assert_eq!(
            g2.check_address(&w, sa("127.0.0.1:7070")),
            AddressVerdict::Block(BlockReason::PuddleEndpoint)
        );
    }

    #[test]
    fn without_signals_the_category_is_empty() {
        let (g, _api) = guard(LocalAccess::NONE);
        let host = named("nexus.corp.example");
        assert_eq!(
            g.check_address_named(&workspace(), Some(&host), sa("100.64.3.7:443")),
            AddressVerdict::Block(BlockReason::LocalToggle(LocalCategory::Private))
        );
        assert_eq!(g.company_signal(Some(&host), sa("10.1.2.3:443")), None);
    }

    #[test]
    fn unreadable_settings_say_so_for_a_company_address_too() {
        let g = company_guard(LocalAccess::UNREADABLE);
        assert_eq!(
            g.check_address(&workspace(), sa("100.64.3.7:443")),
            AddressVerdict::Block(BlockReason::SettingsUnreadable)
        );
    }

    #[test]
    fn a_literal_company_address_is_blocked_before_the_rules() {
        let g = company_guard(LocalAccess::NONE);
        let literal = normalise_host("100.64.3.7").unwrap();
        assert_eq!(
            g.check_target(&workspace(), &literal, 443),
            Some(BlockReason::CompanyNetwork(
                CompanySignal::PrivateAccessCgnat
            ))
        );
        // A name is only known by its address, after the rules.
        let name = normalise_host("nexus.corp.example").unwrap();
        assert_eq!(g.check_target(&workspace(), &name, 443), None);
    }

    #[test]
    fn the_audit_signal_is_reported_for_allowed_and_refused_addresses() {
        let on = company_guard(LocalAccess::NONE.with_toggle(LocalCategory::CompanyNetwork, true));
        let host = named("nexus.corp.example");
        assert_eq!(
            on.company_signal(Some(&host), sa("10.1.2.3:443")),
            Some(CompanySignal::DnsSuffix)
        );
        assert_eq!(on.company_signal(Some(&host), sa("8.8.8.8:443")), None);
    }

    #[test]
    fn the_company_refusal_names_the_switch_and_does_not_promise_access() {
        let host = named("nexus.corp.example");
        let ip: IpAddr = "100.64.3.7".parse().unwrap();
        assert_eq!(
            company_network_message(&host, Some(ip), Some(CompanySignal::VpnRoute)),
            "nexus.corp.example (100.64.3.7) is on your company network (reached through your VPN). Turn on 'Company network' for this workspace to be able to allow it"
        );
        // No signal, no address: the brackets are left out.
        assert_eq!(
            company_network_message(&host, None, None),
            "nexus.corp.example is on your company network. Turn on 'Company network' for this workspace to be able to allow it"
        );
        // An IP literal is not repeated in brackets.
        let literal = named("100.64.3.7");
        assert!(
            company_network_message(&literal, Some(ip), None)
                .starts_with("100.64.3.7 is on your company network.")
        );
        // block_message gives the same text for the block reason.
        assert_eq!(
            block_message(
                &host,
                &workspace(),
                &[BlockReason::CompanyNetwork(CompanySignal::DnsSuffix)]
            ),
            company_network_message(&host, None, Some(CompanySignal::DnsSuffix))
        );
    }
}
