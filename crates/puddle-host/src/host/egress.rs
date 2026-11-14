// SPDX-License-Identifier: AGPL-3.0-or-later
//! Network plumbing the host wires up: corporate roots, injection and the egress proxy.

use crate::agent_reports::ReportDesk;
use std::path::PathBuf;
use std::sync::Arc;

use puddle_api::{EventHub, HostNetworkHealth, ProxySignInService, SettingsRepo};
use puddle_certs::CorporateRoots;
use puddle_compute::Runtime;
use puddle_netpolicy::{
    CompanySignals, NetPolicy, NoSignals, PkiDistributionPoints, PuddleEndpoints,
};
use puddle_ports::Ports;
use puddle_proxy::ssh_agent::SshAgents;
use puddle_proxy::{
    GitSsh, GitSshAccounts, GitSshHttps, Proxy, TcpPortProbe, Terminations, Upstream,
};
use puddle_repos::{HostApi, Repos};
use puddle_secrets::{ReadLog, SecretCache, SecretStore, Sources, ToolPaths};
use puddle_store::{ConnectionRecorder, Store, SystemClock};
use puddle_types::{ConnectionLog, Event, EventSink, Problem, Problems, WorkspaceId};
use puddle_upstream::{AuthList, BasicAuth, Chain, ChainConfig, Discovery, TlsClient, system_auth};

use crate::injection::{Injection, InjectorInputs, credential_read_log, forward_sign_in_needed};
use crate::{HostConfig, HostError, HostWorkspaces, ProxyPasswords};

use super::{AbortOnDrop, HostOptions, local_access};

/// A company root as the TLS client takes it.
pub(super) fn der_of(
    root: &puddle_certs::SyncedCert,
) -> rustls::pki_types::CertificateDer<'static> {
    rustls::pki_types::CertificateDer::from(root.der().to_vec())
}

/// The plain-HTTP certificate distribution points the synced roots name (CRL distribution points,
/// issuer and OCSP addresses): the one destination class that is let through without a rule on a
/// company or private address.
pub(super) fn distribution_points(roots: &CorporateRoots) -> PkiDistributionPoints {
    PkiDistributionPoints::from_urls(roots.distribution_urls())
}

/// The company roots as PEM, for the runtime's registry client.
pub(super) fn pem_of(roots: &CorporateRoots) -> Vec<String> {
    roots
        .certificates()
        .iter()
        .map(puddle_certs::SyncedCert::pem)
        .collect()
}

/// What the network-health screen reads: what discovery, the sign-in log of `chain` and the
/// company roots know. The pull proxy reports itself once it serves.
pub(super) fn network_health_of(
    chain: &Arc<Chain>,
    discovery: &Arc<Discovery>,
    clock: &Arc<SystemClock>,
    roots: &CorporateRoots,
    sign_ins: Arc<dyn ProxySignInService>,
    signals: Arc<dyn CompanySignals>,
) -> Arc<HostNetworkHealth> {
    let health = Arc::new(
        HostNetworkHealth::new(discovery.clone(), clock.clone())
            .with_chain(chain.clone())
            .with_sign_ins(sign_ins)
            .with_company_signals(signals),
    );
    health.set_roots(Arc::new(roots.clone()));
    health
}

/// Makes a repository list that was renewed behind a screen's back an event, so the screen asks
/// again.
pub(super) fn announce_renewals(repos: &Repos, events: Arc<dyn EventSink>) {
    repos.on_change(Arc::new(move || events.emit(Event::RepoListsChanged {})));
}

/// What credential injection is made of, built once at start.
pub(super) struct Injecting {
    pub(super) injection: Arc<Injection>,
    pub(super) secrets: Arc<SecretCache<Sources>>,
    /// What the last read of each credential came to, by every use; shown on the identities.
    pub(super) reads: Arc<ReadLog>,
    pub(super) terminations: Arc<Terminations>,
    pub(super) tls: TlsClient,
}

impl Injecting {
    /// What the egress proxy terminates with: the registry it asks and the TLS client.
    pub(super) fn termination(&self) -> (Arc<Terminations>, TlsClient) {
        (self.terminations.clone(), self.tls.clone())
    }

    /// The repository lists: read from the Git hosts' APIs over the company-proxy route with the
    /// secrets the proxy's injection uses.
    pub(super) fn repos(
        &self,
        chain: &Arc<Chain>,
        clock: &Arc<SystemClock>,
        log: Arc<dyn ConnectionLog>,
        events: &Arc<EventHub>,
    ) -> Arc<Repos> {
        let repos = Arc::new(Repos::new(
            // Listing repositories is Puddle's own traffic: audited with origin `puddle`, like an
            // image pull.
            Arc::new(HostApi::new(chain.clone(), self.tls.clone()).with_connection_log(log)),
            self.secrets.clone(),
            clock.clone(),
        ));
        announce_renewals(&repos, events.clone());
        repos
    }

    /// Starts following the changes that reach a running workspace: its identities (what it
    /// decrypts, its commit authors) and the sign-ins its credentials need.
    pub(super) fn watch<R: Runtime + Clone>(
        &self,
        service: &HostWorkspaces<R>,
        events: &Arc<EventHub>,
    ) -> (AbortOnDrop, AbortOnDrop) {
        (
            AbortOnDrop(crate::changes::follow(events.subscribe(), service.clone())),
            AbortOnDrop(forward_sign_in_needed(
                &self.secrets,
                events.clone() as Arc<dyn EventSink>,
            )),
        )
    }

    pub(super) fn build(
        options: &HostOptions,
        vault: &Arc<dyn SecretStore>,
        store: &Arc<Store>,
        events: &Arc<EventHub>,
        roots: &CorporateRoots,
        login_markers: PathBuf,
        settings: &Arc<dyn SettingsRepo>,
    ) -> Result<Self, HostError> {
        // The secrets read from the user's own sign-ins (`gh`, Git, the operating system's store).
        let reads = credential_read_log(events.clone() as Arc<dyn EventSink>);
        let secrets = Arc::new(
            SecretCache::new(Sources::new(ToolPaths::resolve(), Arc::clone(vault)))
                .with_read_log(Arc::clone(&reads)),
        );
        // Values nobody asks for any more leave memory on their own.
        if tokio::runtime::Handle::try_current().is_ok() {
            drop(secrets.spawn_sweeper());
        }
        // The proxy asks this registry, once per connection, what the workspace decrypts.
        let terminations = Arc::new(Terminations::new());
        let injection = Arc::new(
            Injection::new(
                terminations.clone(),
                InjectorInputs {
                    store: store.clone(),
                    secrets: secrets.clone(),
                    events: events.clone() as Arc<dyn EventSink>,
                },
                options.injector.clone(),
                Arc::clone(vault),
            )
            .with_login_markers(login_markers)
            .with_settings(Arc::clone(settings))
            .with_git_ssh(options.git_ssh),
        );
        // The platform's roots plus the company's, as the guest is given.
        let tls = TlsClient::new(roots.certificates().iter().map(der_of))?;
        Ok(Self {
            injection,
            secrets,
            reads,
            terminations,
            tls,
        })
    }
}

/// Reads the stored proxy sign-ins off the async threads: a locked credential store may take a
/// long time to answer and starting must not wait for it. Until it has, a proxy that asks for Basic
/// is answered as if no sign-in were stored.
pub(super) fn load_proxy_passwords(passwords: Arc<ProxyPasswords>, problems: Arc<Problems>) {
    const PROBLEM: &str = "proxy-sign-in-store";
    tokio::task::spawn_blocking(move || match passwords.load() {
        Ok(count) => {
            tracing::debug!(count, "proxy sign-ins loaded");
            problems.clear(PROBLEM);
        }
        Err(_) => problems.raise(Problem::new(
            PROBLEM,
            "Puddle could not read the company proxy sign-ins",
            "The operating system's credential store did not answer, so a proxy that asks for a \
             user name and password is not signed in to. Unlock the credential store and restart \
             Puddle.",
        )),
    });
}

/// What the way out is given besides the settings: the machinery for decrypted hosts and the stored
/// proxy sign-ins.
pub(super) struct EgressParts {
    pub(super) terminations: Arc<Terminations>,
    pub(super) tls: TlsClient,
    pub(super) proxy_passwords: Arc<ProxyPasswords>,
    /// Where the guest agents' reports go.
    pub(super) reports: Arc<ReportDesk>,
    pub(super) ports: Ports,
    /// Git over SSH, with the accounts `ssh -T` greets; `None` leaves SSH to Git hosts refused.
    pub(super) git_ssh: Option<Arc<crate::ssh_accounts::SshAccounts>>,
    /// The certificate distribution points the synced roots name.
    pub(super) pki: PkiDistributionPoints,
}

impl EgressParts {
    pub(super) fn new(
        (terminations, tls): (Arc<Terminations>, TlsClient),
        proxy_passwords: &Arc<ProxyPasswords>,
        reports: &Arc<ReportDesk>,
        ports: &Ports,
        roots: &CorporateRoots,
    ) -> Self {
        Self {
            terminations,
            tls,
            proxy_passwords: proxy_passwords.clone(),
            reports: reports.clone(),
            ports: ports.clone(),
            git_ssh: None,
            pki: distribution_points(roots),
        }
    }
}

/// The company-network signals the host was given, or none (the category is then empty).
pub(super) fn company_signals(options: &HostOptions) -> Arc<dyn CompanySignals> {
    options
        .company_signals
        .clone()
        .unwrap_or_else(|| Arc::new(NoSignals))
}

/// The way out through the company network, shared by the sandbox proxy and the pull proxy.
pub(super) struct Egress {
    pub(super) proxy: Arc<Proxy>,
    pub(super) upstream: Upstream,
    pub(super) chain: Arc<Chain>,
    pub(super) discovery: Arc<Discovery>,
}

impl Egress {
    pub(super) fn build(
        config: &HostConfig,
        options: &HostOptions,
        settings: &Arc<dyn SettingsRepo>,
        endpoints: &PuddleEndpoints,
        connections: &Arc<ConnectionRecorder>,
        events: &Arc<EventHub>,
        parts: EgressParts,
    ) -> Self {
        let EgressParts {
            terminations,
            tls,
            proxy_passwords,
            reports,
            ports,
            git_ssh,
            pki,
        } = parts;
        let discovery = options.discovery.clone().unwrap_or_else(|| {
            Discovery::new(
                puddle_upstream::system_os(),
                config.upstream.discovery.clone(),
            )
        });
        // Basic: the sign-in stored for the proxy, else the one configured for every proxy.
        let mut basic = BasicAuth::new().with_source(proxy_passwords);
        if let Some(credentials) = config.upstream.basic.clone() {
            basic = basic.with_default(credentials);
        }
        let auth = AuthList::new().with(system_auth()).with(Arc::new(basic));
        let chain = match options.host_lookup.clone() {
            Some(lookup) => Chain::with_lookup(
                discovery.clone(),
                Arc::new(auth),
                ChainConfig::default(),
                lookup,
            ),
            None => Chain::new(discovery.clone(), Arc::new(auth)),
        };
        let upstream = Upstream::new(chain.clone());
        let access_settings = settings.clone();
        let guard = NetPolicy::new(Arc::new(move |workspace: &WorkspaceId| {
            local_access(access_settings.as_ref(), workspace)
        }))
        .with_endpoints(endpoints.clone())
        .with_company_signals(company_signals(options));
        let mut proxy = Proxy::new(
            connections.store().clone(),
            events.clone() as Arc<dyn EventSink>,
        )
        .with_connection_log(connections.clone())
        .with_address_check(Arc::new(guard))
        .with_upstream(upstream.clone())
        .with_termination(terminations, tls)
        .with_agent_reports(reports)
        .with_guest_watcher(Arc::new(ports))
        .with_pki_distribution_points(pki)
        .with_port_probe(
            options
                .port_probe
                .clone()
                .unwrap_or_else(|| Arc::new(TcpPortProbe::default())),
        )
        // On for no host until the user turns it on for one; the hosts are each workspace's.
        .with_ssh_agent(Arc::new(SshAgents::new(Arc::new(
            crate::ssh_agent::SettingsAgent::new(Some(Arc::clone(settings))),
        ))));
        if let Some(resolver) = &options.resolver {
            proxy = proxy.with_resolver(Arc::clone(resolver));
        }
        if let Some(accounts) = git_ssh {
            // Each workspace serves its own start's host keys (`Injection`), so none here.
            let https = GitSshHttps::new(accounts as Arc<dyn GitSshAccounts>);
            proxy = proxy.with_git_ssh(GitSsh::new(Arc::new(https), Vec::new()));
        }
        let proxy = Arc::new(proxy);
        Self {
            proxy,
            upstream,
            chain,
            discovery,
        }
    }
}
