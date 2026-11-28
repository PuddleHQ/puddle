// SPDX-License-Identifier: AGPL-3.0-or-later
//! The phases of `Host::start`, each a function with the inputs it needs.

use crate::agent_reports::ReportDesk;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use puddle_api::{EventHub, Services, SettingsRepo};
use puddle_certs::CorporateRoots;
use puddle_compute::Runtime;
use puddle_front::Front;
use puddle_lifecycle::Lifecycle;
use puddle_netpolicy::PuddleEndpoints;
use puddle_ports::Ports;
use puddle_proxy::{ProxyUrl, PullProxy, PullRoute};
use puddle_secrets::{SecretStore, ToolPaths};
use puddle_services::{
    credentials::HostCredentials, network_health::HostNetworkHealth,
    network_health::PullProxyState, proxy_sign_in::ProxySignInService,
};
use puddle_store::{
    AllUsers, ConnectionRecorder, DEFAULT_SWEEP_PERIOD, Limits, Store, Sweeper, SystemClock,
};
use puddle_types::{Problems, WorkspaceId, WorkspaceStatus};
use puddle_upstream::Watching;
use puddle_workspace::Workspaces;

use crate::backup::DataFolderBackup;
use crate::boot::BootKit;
use crate::dev_cert::{DevCertSource, host_dev_cert};
use crate::doctor::system_doctor;
use crate::files::{FileSettings, WorkspaceBook};
use crate::front::Flavours;
use crate::learned::HostLearned;
use crate::ports::{HostPorts, StorePins};
use crate::proxy_sign_ins::HostProxySignIns;
use crate::quit_trims::QuitTrims;
use crate::restore::DataFolderRestore;
use crate::ssh_config::SshSetup;
use crate::workspaces::Parts;
use crate::{HostConfig, HostError, HostWorkspaces, ProxyPasswords};

use super::background::AbortOnDrop;
use super::egress::{
    Egress, EgressParts, Injecting, company_signals, load_proxy_passwords, network_health_of,
    pem_of,
};
use super::reconcile::{reconcile_problems, reconcile_with};
use super::{HostOptions, RuntimeFactory, RuntimeInputs, Step};

/// What `open_state` opened.
pub(super) struct State {
    pub(super) store: Arc<Store>,
    /// The reach across every user's data, taken before anything serves.
    pub(super) all_users: AllUsers,
    pub(super) settings: Arc<dyn SettingsRepo>,
    pub(super) book: WorkspaceBook,
    pub(super) stored: Vec<crate::files::Stored>,
    pub(super) retired: BTreeSet<WorkspaceId>,
    pub(super) cleaned: BTreeSet<WorkspaceId>,
}

/// Opens the database, the settings files and the workspace list.
pub(super) fn open_state(
    paths: &crate::HostPaths,
    events: &Arc<EventHub>,
    problems: &Arc<Problems>,
    clock: &Arc<SystemClock>,
    steps: &mut Vec<Step>,
) -> Result<State, HostError> {
    crate::data_folder::make_owner_only(paths)?;
    // A restore the user confirmed before the last stop is applied here: the host alone holds the
    // data folder and the store is not open yet.
    if let Some(outcome) = crate::restore::apply::apply_pending(paths)
        && !outcome.applied
    {
        problems.raise(puddle_types::Problem::new(
            "restore-failed",
            "A restore from a backup failed and was undone",
            outcome.message,
        ));
    }
    let store = Arc::new(
        Store::open(&paths.store(), clock.clone(), Limits::default())?.with_events(events.clone()),
    );
    // Taken here, before the store reaches anything that serves requests, so no request handler
    // can take it after.
    let all_users = store.take_all_users().ok_or(HostError::State {
        what: "the database",
        reason: "the reach across every user was taken before the host started".to_owned(),
    })?;
    let settings: Arc<dyn SettingsRepo> = Arc::new(FileSettings::new(paths.settings()));
    let book = WorkspaceBook::new(paths.workspace_book());
    let crate::files::Book {
        workspaces: stored,
        retired,
        cleaned,
    } = book.load(&store.workspace_owners()?)?;
    steps.push(Step::StoreOpened);
    Ok(State {
        store,
        all_users,
        settings,
        book,
        stored,
        retired,
        cleaned,
    })
}

/// The handles every start-up phase reads: the settings it has opened and the options it was given.
pub(super) struct Shared<'a> {
    pub(super) config: &'a HostConfig,
    pub(super) options: &'a HostOptions,
    pub(super) events: &'a Arc<EventHub>,
    pub(super) problems: &'a Arc<Problems>,
    pub(super) clock: &'a Arc<SystemClock>,
    pub(super) store: &'a Arc<Store>,
    pub(super) settings: &'a Arc<dyn SettingsRepo>,
    pub(super) endpoints: &'a PuddleEndpoints,
    pub(super) roots: &'a CorporateRoots,
    /// Told when a restore is confirmed (see [`Host::restore_requested`]).
    pub(super) restore_requested: &'a Arc<tokio::sync::Notify>,
}

/// The way out through the company network, with what the host's screens read about it.
pub(super) struct Outbound {
    pub(super) connections: Arc<ConnectionRecorder>,
    pub(super) reports: Arc<ReportDesk>,
    pub(super) ports: Ports,
    pub(super) ssh_accounts: Arc<crate::ssh_accounts::SshAccounts>,
    pub(super) egress: Egress,
    pub(super) sign_ins: Arc<dyn ProxySignInService>,
    pub(super) network_health: Arc<HostNetworkHealth>,
    pub(super) pull_url: ProxyUrl,
}

/// The way out: the company network (discovery, sign-in as the user, then Basic) for the sandbox
/// proxy and the pull proxy alike. Returns the pull proxy, now going out through it.
pub(super) fn build_outbound(
    shared: &Shared<'_>,
    injecting: &Injecting,
    proxy_passwords: &Arc<ProxyPasswords>,
    pull: PullProxy,
) -> (Outbound, PullProxy) {
    let Shared {
        config,
        options,
        events,
        problems,
        clock,
        store,
        settings,
        endpoints,
        roots,
        ..
    } = *shared;
    let connections = Arc::new(ConnectionRecorder::new(store.clone(), problems.clone()));
    let reports = Arc::new(ReportDesk::new(problems.clone()));
    let ports = Ports::new(
        endpoints.clone(),
        Arc::new(StorePins::new(store.clone())),
        events.clone(),
    )
    .with_auto_forward();
    // Who `ssh -T` greets: read from the store, named once the repository reader exists.
    let ssh_accounts = Arc::new(crate::ssh_accounts::SshAccounts::new(store.clone()));
    let mut egress_parts = EgressParts::new(
        injecting.termination(),
        proxy_passwords,
        &reports,
        &ports,
        roots,
    );
    if options.git_ssh {
        egress_parts.git_ssh = Some(ssh_accounts.clone());
    }
    let egress = Egress::build(
        config,
        options,
        settings,
        endpoints,
        &connections,
        events,
        egress_parts,
    );
    let sign_ins: Arc<dyn ProxySignInService> = Arc::new(HostProxySignIns::new(
        proxy_passwords.clone(),
        egress.chain.clone(),
        problems.clone(),
    ));
    let network_health = network_health_of(
        &egress.chain,
        &egress.discovery,
        clock,
        roots,
        sign_ins.clone(),
        company_signals(options),
    );
    network_health.set_tls_left_out(injecting.tls.rejected_roots(), roots);
    let pull_url = pull.proxy_url();
    // Image pulls are Puddle's own traffic: audited with origin `puddle`, like sandbox traffic.
    let pull = pull
        .with_company_signals(company_signals(options))
        .with_upstream(egress.upstream.clone())
        .with_connection_log(connections.clone());
    let outbound = Outbound {
        connections,
        reports,
        ports,
        ssh_accounts,
        egress,
        sign_ins,
        network_health,
        pull_url,
    };
    (outbound, pull)
}

/// What the credential injection and the proxy sign-ins stand on.
pub(super) struct Credentials {
    pub(super) vault: Arc<dyn SecretStore>,
    pub(super) vault_sweep: tokio::task::JoinHandle<()>,
    pub(super) store_retry: Arc<crate::vault::StoreRetry>,
    pub(super) injecting: Injecting,
    pub(super) proxy_passwords: Arc<ProxyPasswords>,
}

/// Credential injection: what each workspace decrypts, the injector and the secrets behind it,
/// and the client that verifies the real servers of decrypted hosts. `listed` are the workspaces
/// in the book, whose saved logins the clean-up of the store keeps.
pub(super) async fn open_credentials(
    shared: &Shared<'_>,
    listed: Vec<WorkspaceId>,
) -> Result<Credentials, HostError> {
    let Shared {
        config,
        options,
        events,
        problems,
        store,
        settings,
        roots,
        ..
    } = *shared;
    let chosen = crate::vault::open(
        options.secret_store.as_ref(),
        &options.secret_setup,
        &config.paths,
        store,
        problems,
    )
    .await?;
    let vault: Arc<dyn SecretStore> = chosen.vault.clone();
    chosen.check_in_background(problems.clone());
    let store_retry = Arc::new(chosen.retry_check(problems.clone()));
    let vault_sweep = if chosen.usable {
        crate::vault::sweep_in_background(vault.clone(), store.clone(), listed, problems.clone())
    } else {
        tokio::task::spawn_blocking(|| ())
    };
    let marks = config.paths.login_markers();
    let injecting = Injecting::build(options, &vault, store, events, roots, marks, settings)?;
    let proxy_passwords =
        ProxyPasswords::with_known_file(vault.clone(), config.paths.proxy_sign_in_names());
    load_proxy_passwords(proxy_passwords.clone(), problems.clone());
    Ok(Credentials {
        vault,
        vault_sweep,
        store_retry,
        injecting,
        proxy_passwords,
    })
}

/// The guests' boot files and the runtime that boots them.
pub(super) struct Opened<R> {
    pub(super) dev_cert: Arc<dyn DevCertSource>,
    pub(super) kit: BootKit,
    pub(super) ssh: Arc<SshSetup>,
    pub(super) runtime: R,
}

pub(super) async fn open_runtime<R, F>(
    shared: &Shared<'_>,
    outbound: &Outbound,
    factory: &F,
    steps: &mut Vec<Step>,
) -> Result<Opened<R>, HostError>
where
    R: Runtime,
    F: RuntimeFactory<Runtime = R>,
{
    let Shared {
        config,
        options,
        clock,
        store,
        roots,
        ..
    } = *shared;
    // One development certificate service: the guests' boot files and the first-run line.
    let dev_cert: Arc<dyn DevCertSource> = options
        .dev_cert
        .clone()
        .unwrap_or_else(|| host_dev_cert(config.paths.data(), clock.clone()));
    let kit = BootKit::new(
        &config.paths.guest_share(),
        &config.guest,
        outbound.egress.proxy.clone(),
        roots,
        store.clone(),
        config.paths.data(),
    )?
    .with_dev_cert(dev_cert.clone());
    steps.push(Step::GuestFilesReady);

    // The login key of direct SSH: made before the runtime opens, which takes its public half.
    let ssh = Arc::new(SshSetup::open(config.paths.data(), &config.ssh));

    // The runtime, with image pulls through the pull proxy and the corporate roots trusted.
    let runtime = factory
        .open(RuntimeInputs {
            layout: &config.layout,
            guest_share: config.paths.guest_share(),
            data: config.paths.data().to_path_buf(),
            pull_proxy: outbound.pull_url.clone(),
            registry_roots: pem_of(roots),
            log_level: config.runtime_log_level.clone(),
            ssh_login_key: ssh.authorized_key().map(str::to_owned),
        })
        .await?;
    steps.push(Step::RuntimeOpened);
    Ok(Opened {
        dev_cert,
        kit,
        ssh,
        runtime,
    })
}

/// The parts of the host that serve beside the API.
pub(super) struct Serving {
    pub(super) pull: PullRoute,
    pub(super) sweeper: Sweeper,
    pub(super) watching: Option<Watching>,
    pub(super) network_events: AbortOnDrop,
    pub(super) sign_in_failures: AbortOnDrop,
    pub(super) flavours: Arc<Flavours>,
    pub(super) front: Front,
    pub(super) front_guests: Arc<dyn puddle_front::Guests>,
    pub(super) front_halves: Arc<dyn puddle_front::ServerHalves>,
}

/// The pull proxy first: the first create pulls an image. Then the sweeper, the network watch and
/// the browser editor's front.
pub(super) fn serve_background(
    shared: &Shared<'_>,
    outbound: &Outbound,
    pull: PullProxy,
    steps: &mut Vec<Step>,
) -> Result<Serving, HostError> {
    let pull = pull.serve().map_err(HostError::PullProxy)?;
    // Image pulls leave through the company proxy chain, like the sandboxes' traffic.
    outbound
        .network_health
        .set_pull_proxy(PullProxyState::ViaUpstream);
    steps.push(Step::PullProxyServing);
    let sweeper = Sweeper::spawn_reporting(
        shared.store.clone(),
        DEFAULT_SWEEP_PERIOD,
        shared.problems.clone(),
    );
    let watching = outbound.egress.discovery.watch();
    let network_events = AbortOnDrop::forwarding(&outbound.egress.discovery, shared.events);
    let sign_in_failures =
        AbortOnDrop::forwarding_sign_in_failures(&outbound.egress.chain, shared.events);
    // The browser editor's front: a listener on this computer's loopback that reaches each
    // running workspace's editor through its agent link, so a window can open it by origin.
    let flavours = Arc::new(Flavours::default());
    let (front, front_guests, front_halves) = crate::front::serve(
        &shared.config.paths,
        shared.endpoints,
        outbound.ports.clone(),
        flavours.clone(),
        shared.problems,
    )?;
    steps.push(Step::BackgroundStarted);
    Ok(Serving {
        pull,
        sweeper,
        watching,
        network_events,
        sign_in_failures,
        flavours,
        front,
        front_guests,
        front_halves,
    })
}

/// What a run of the host left unfinished, and what it learned about it: the reconcile report, the
/// state of each known sandbox and the trims that failed when the last run quit.
pub(super) struct Reconciled {
    pub(super) report: puddle_lifecycle::ReconcileReport,
    pub(super) status: BTreeMap<WorkspaceId, WorkspaceStatus>,
    pub(super) quit_trims: QuitTrims,
    pub(super) carried: BTreeMap<WorkspaceId, String>,
}

/// Cleans up after an earlier run, then reads what only lives in memory back in.
pub(super) async fn reconcile_phase<R: Runtime>(
    runtime: &R,
    config: &HostConfig,
    stored: &[crate::files::Stored],
    problems: &Problems,
) -> Result<Reconciled, HostError> {
    let (report, status) = reconcile_with(runtime, config, stored).await?;
    for problem in reconcile_problems(&report) {
        problems.raise(problem);
    }
    // A trim that failed when the last run quit is told now, to a user who can read it.
    let quit_trims = QuitTrims::new(config.paths.quit_trims());
    let carried: BTreeMap<WorkspaceId, String> = quit_trims
        .load()
        .into_iter()
        .filter(|(id, _)| stored.iter().any(|s| &s.id == id))
        .collect();
    for (id, why) in &carried {
        if let Some(stored) = stored.iter().find(|s| &s.id == id) {
            problems.raise(crate::quit_trims::problem(id, stored.name.as_str(), why));
        }
    }
    Ok(Reconciled {
        report,
        status,
        quit_trims,
        carried,
    })
}

/// What the workspace service is made of, from the phases before it.
pub(super) fn workspace_parts<R: Runtime + Clone>(
    shared: &Shared<'_>,
    outbound: &Outbound,
    injecting: &Injecting,
    serving: &Serving,
    opened: Opened<R>,
    (workspaces, book, all_users): (Workspaces, WorkspaceBook, AllUsers),
    lifecycle: Arc<Lifecycle<R>>,
) -> Parts<R> {
    let Shared {
        config,
        options,
        events,
        problems,
        clock,
        store,
        settings,
        ..
    } = *shared;
    Parts {
        runtime: opened.runtime,
        workspaces,
        lifecycle,
        kit: opened.kit,
        events: events.clone(),
        clock: clock.clone(),
        settings: settings.clone(),
        store: store.clone(),
        all_users,
        launcher: options.launcher.clone(),
        ssh: opened.ssh,
        book,
        injection: injecting.injection.clone(),
        problems: problems.clone(),
        reports: outbound.reports.clone(),
        ports: outbound.ports.clone(),
        rewrite_gate: options.rewrite_gate.clone(),
        flavours: serving.flavours.clone(),
        front_addr: serving.front.local_addr(),
        front_guests: serving.front_guests.clone(),
        front_halves: serving.front_halves.clone(),
        ids: options
            .workspace_ids
            .clone()
            .unwrap_or_else(|| Arc::new(puddle_services::workspaces::RandomIds)),
        login_markers: config.paths.login_markers(),
        space: crate::space_watch::space_or_drive(options.free_space.clone(), config.paths.data()),
    }
}

/// The services the API serves, wired to the host's stores and workspaces.
pub(super) fn build_services<R: Runtime + Clone>(
    shared: &Shared<'_>,
    service: &HostWorkspaces<R>,
    credentials: &Credentials,
    outbound: &Outbound,
    dev_cert: Arc<dyn DevCertSource>,
) -> Services {
    let Credentials {
        vault,
        store_retry,
        injecting,
        ..
    } = credentials;
    let vault = vault.clone();
    let Shared {
        config,
        options,
        events,
        problems,
        clock,
        store,
        settings,
        endpoints,
        ..
    } = *shared;
    let services = Services::new(
        store.clone(),
        settings.clone(),
        events.clone(),
        clock.clone(),
    )
    .with_problems(problems.clone())
    .with_workspaces(Arc::new(service.clone()))
    .with_network_health(outbound.network_health.clone())
    .with_secret_store(vault.clone())
    .with_credentials(Arc::new(
        HostCredentials::new(ToolPaths::resolve(), vault)
            .with_read_log(Arc::clone(&injecting.reads)),
    ))
    .with_repos({
        let repos = injecting.repos(
            &outbound.egress.chain,
            clock,
            outbound.connections.clone(),
            events,
        );
        outbound.ssh_accounts.read_with(repos.clone());
        repos
    })
    .with_backup(Arc::new(DataFolderBackup::new(config.paths.clone())))
    .with_restore(Arc::new(DataFolderRestore::new(
        config.paths.clone(),
        shared.restore_requested.clone(),
    )))
    .with_doctor(system_doctor(
        &config.layout,
        &config.paths,
        &config.expected_runtime,
        settings.clone(),
        options.az_search_path.clone(),
    ))
    .with_dev_cert(dev_cert)
    .with_credential_store(store_retry.clone())
    .with_ports(Arc::new(HostPorts::new(outbound.ports.clone())))
    .with_terminals(Arc::new(service.clone()))
    .with_learned(Arc::new(HostLearned::new(
        injecting.terminations.clone(),
        Arc::clone(store),
    )))
    .with_proxy_sign_ins(outbound.sign_ins.clone())
    .with_company_signals(company_signals(options))
    .with_endpoints(endpoints.clone());
    let services = options.front_resolver.iter().fold(services, |s, resolver| {
        s.with_front_resolver(resolver.clone())
    });
    // Browsers sign in against the database's secrets, which only the API ever holds.
    match store.take_account_secrets() {
        Some(secrets) => services.with_account_secrets(secrets),
        None => services,
    }
}
