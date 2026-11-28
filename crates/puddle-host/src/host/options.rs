// SPDX-License-Identifier: AGPL-3.0-or-later
//! What a host takes besides its configuration.

use crate::agent_reports::ReportDesk;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, PoisonError};

use puddle_api::{EventHub, RunningApi, Services, SettingsRepo};
use puddle_certs::CorporateRoots;
use puddle_compute::Runtime;
use puddle_front::Front;
use puddle_fs::DataLock;
use puddle_lifecycle::{Lifecycle, ShutdownReport};
use puddle_netpolicy::{CompanySignals, LocalAccess, PuddleEndpoints};
use puddle_ports::Ports;
use puddle_proxy::{PortProbe, ProxyUrl, PullProxy, PullRoute, Resolver};
use puddle_secrets::{SecretStore, ToolPaths};
use puddle_services::{
    credentials::HostCredentials, network_health::HostNetworkHealth,
    network_health::PullProxyState, network_health::forward_network_changes,
    network_health::forward_proxy_sign_in_failures, proxy_sign_in::ProxySignInService,
    workspaces::Launcher,
};
use puddle_settings::resolve;
use puddle_store::{
    AllUsers, ConnectionRecorder, DEFAULT_SWEEP_PERIOD, Limits, Store, Sweeper, SystemClock,
};
use puddle_types::{Problems, WorkspaceId, WorkspaceStatus};
use puddle_upstream::{Chain, Discovery, Watching};
use puddle_workspace::Workspaces;
use tokio::sync::{Mutex as AsyncMutex, OnceCell};
use url::Url;

use crate::backup::DataFolderBackup;
use crate::boot::BootKit;
use crate::dev_cert::{DevCertSource, host_dev_cert};
use crate::doctor::system_doctor;
use crate::files::{FileSettings, WorkspaceBook};
use crate::front::Flavours;
use crate::injection::InjectorFactory;
use crate::learned::HostLearned;
use crate::ports::{HostPorts, StorePins};
use crate::proxy_sign_ins::HostProxySignIns;
use crate::quit_trims::QuitTrims;
use crate::restore::DataFolderRestore;
use crate::ssh_config::SshSetup;
use crate::workspaces::{Parts, RewriteGate};
use crate::{HostConfig, HostError, HostWorkspaces, ProxyPasswords, VsCodeLauncher};

/// What a host takes besides its configuration.
#[derive(Clone)]
#[non_exhaustive]
pub struct HostOptions {
    /// Opens the desktop editor for the workspaces: VS Code's `code` command by default.
    pub launcher: Arc<dyn Launcher>,
    /// The proxy discovery to use instead of the system's (tests).
    pub discovery: Option<Arc<Discovery>>,
    /// Builds, for each start of a workspace's sandbox, the injector that decides which credential
    /// is added to a request on a decrypted host. Without one a workspace gets Puddle's own Git
    /// injector (tests give another, for instance one that adds nothing).
    pub injector: Option<InjectorFactory>,
    /// Where the values of secrets are kept: the pasted tokens of identities and the secrets of
    /// workspace environments. Without one, the operating system's credential store (a value too
    /// long for one of its entries is spread over several). Tests give an in-memory store, so they
    /// never touch the real one.
    pub secret_store: Option<Arc<dyn SecretStore>>,
    /// How the store is chosen when `secret_store` is `None`: the `secret_store` option, the
    /// headless flag and the places the choice reads. The defaults are the shipped host's.
    pub secret_setup: crate::vault::SecretSetup,
    /// Where the development certificate comes from: the first-run state and the certificate every
    /// workspace gets at boot. Without one, the host's own (its certificate stores and the data
    /// folder). Tests give a double.
    pub dev_cert: Option<Arc<dyn DevCertSource>>,
    /// Looks up the addresses of a name a rule allows, for the sandbox proxy's connects. Without
    /// one, the operating system's resolver. Tests give a double, so a name only a fake company
    /// proxy knows is never a question for the machine's real DNS.
    pub resolver: Option<Arc<dyn Resolver>>,
    /// What the host reads of the company network (proxy script, DNS suffixes, private-access
    /// clients, routes), shared by the sandbox proxy's guard and the API. Without one the host
    /// reads none, so no destination is ever in the company-network category. The shipped host
    /// uses [`HostOptions::system`], which reads the machine's own.
    pub company_signals: Option<Arc<dyn CompanySignals>>,
    /// Probes whether this machine can open `ssh.dev.azure.com:22`, for the refusal that says the
    /// company network blocks it. Without one, a real TCP probe; tests give a double so no test
    /// depends on the machine's network.
    pub port_probe: Option<Arc<dyn PortProbe>>,
    /// Holds an author rewrite back when it starts (tests, to make the rewrites of one workspace
    /// run out of order). Without one, no rewrite waits.
    pub rewrite_gate: Option<RewriteGate>,
    /// Serves Git over SSH for the Git hosts: SSH host keys for each start of a workspace's
    /// sandbox, listed in the guest's `ssh_known_hosts` under the Git hosts, and Puddle's own SSH
    /// server, which carries each command out over the host's HTTPS with the workspace's
    /// identities. On by default; off, SSH to a Git host is refused (`ssh_unsupported`).
    pub git_ssh: bool,
    /// Where new workspace ids come from. Without one, the operating system's random source
    /// ([`puddle_services::workspaces::RandomIds`]); a test that writes ids into paths itself gives
    /// [`puddle_services::workspaces::NameIds`], so a workspace called `acme` gets the id `acme`.
    pub workspace_ids: Option<Arc<dyn puddle_services::workspaces::IdSource>>,
    /// Where a root disk grow reads the free space of the data folder's drive. Without one, the
    /// real drive; tests give a fixed figure so a grow never depends on the machine's disk.
    pub free_space: Option<Arc<dyn crate::Space>>,
    /// Looks up a name for the host's own connections (the lists of repositories, the sign-in
    /// checks) before the company proxy is asked. Without one, the operating system's resolver;
    /// tests give a double, so a name is never a question for the machine's real DNS.
    pub host_lookup: Option<puddle_upstream::HostLookup>,
    /// Resolves the names of the front in front of Puddle for the guard. Without one, the
    /// operating system's resolver; tests give a double.
    pub front_resolver: Option<Arc<dyn puddle_api::NameResolver>>,
    /// The folders searched for the Azure CLI by the system check (a `PATH` value). Without one,
    /// the process's `PATH`; tests give an empty one, so the check never runs the machine's real
    /// `az`, which waits on the network.
    pub az_search_path: Option<std::ffi::OsString>,
}

impl Default for HostOptions {
    fn default() -> Self {
        Self {
            launcher: Arc::new(VsCodeLauncher::new()),
            discovery: None,
            injector: None,
            secret_store: None,
            secret_setup: crate::vault::SecretSetup::default(),
            dev_cert: None,
            resolver: None,
            company_signals: None,
            port_probe: None,
            rewrite_gate: None,
            git_ssh: true,
            workspace_ids: None,
            free_space: None,
            host_lookup: None,
            front_resolver: None,
            az_search_path: None,
        }
    }
}

impl HostOptions {
    /// The options of the shipped host: the defaults, plus the company-network signals the
    /// machine itself gives ([`crate::HostSignals::system`]; none on a platform without readers).
    /// Tests start from [`HostOptions::default`], so a test never depends on the machine's network.
    #[must_use]
    pub fn system() -> Self {
        Self {
            company_signals: Some(crate::HostSignals::system()),
            ..Self::default()
        }
    }
}

impl std::fmt::Debug for HostOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostOptions").finish_non_exhaustive()
    }
}
