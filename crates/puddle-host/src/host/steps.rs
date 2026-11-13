// SPDX-License-Identifier: AGPL-3.0-or-later
//! The named steps of preparing, starting and shutting down, and the prepared host.

use puddle_certs::CorporateRoots;
use puddle_fs::DataLock;
use puddle_netpolicy::PuddleEndpoints;
use puddle_proxy::PullProxy;

use crate::{HostConfig, HostError, Platform};

/// One step of starting or stopping the host, in the order they ran.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Step {
    /// This process holds the data folder; no other Puddle process can start on it.
    DataFolderLocked,
    /// The image-pull proxy is bound (not serving yet).
    PullProxyBound,
    /// The bundled runtime is present and of the exact version.
    RuntimeChecked,
    /// The corporate root certificates are read.
    RootsRead,
    /// The database is open and the event hub is wired to it.
    StoreOpened,
    /// The way out through the company network is built: discovery, sign-in, the chain.
    UpstreamBuilt,
    /// The files and the egress proxy sandboxes are built from are ready.
    GuestFilesReady,
    /// The sandbox runtime is open (its image pulls go through the pull proxy).
    RuntimeOpened,
    /// What an earlier run left behind is cleaned up.
    Reconciled,
    /// The workspaces' holders are rebuilt from what exists.
    WorkspacesAdopted,
    /// The image-pull proxy serves.
    PullProxyServing,
    /// The sweeper, the network-change watcher and the browser editor's front run.
    BackgroundStarted,
    /// The workspace service is built over the runtime.
    WorkspacesReady,
    /// The API serves.
    ApiServing,
    /// New workspace operations are refused.
    WorkspacesClosed,
    /// Running workspace operations have finished (or were ended after the grace period).
    OperationsFinished,
    /// Every sandbox is trimmed and stopped.
    WorkspacesStopped,
    /// The SSH endpoints and egress routes are closed.
    RoutesClosed,
    /// The API stopped serving.
    ApiStopped,
    /// The sweeper, the watcher, the pull proxy and the front are stopped.
    BackgroundStopped,
}

/// The steps [`prepare`] runs, in order.
pub const PREPARE_STEPS: [Step; 4] = [
    Step::DataFolderLocked,
    Step::PullProxyBound,
    Step::RuntimeChecked,
    Step::RootsRead,
];

/// The steps of a start with the default options, in order, [`prepare`]'s included.
pub const START_STEPS: [Step; 14] = [
    Step::DataFolderLocked,
    Step::PullProxyBound,
    Step::RuntimeChecked,
    Step::RootsRead,
    Step::StoreOpened,
    Step::UpstreamBuilt,
    Step::GuestFilesReady,
    Step::RuntimeOpened,
    Step::Reconciled,
    Step::WorkspacesAdopted,
    Step::PullProxyServing,
    Step::BackgroundStarted,
    Step::WorkspacesReady,
    Step::ApiServing,
];

/// The steps of a shutdown, in order.
pub const SHUTDOWN_STEPS: [Step; 6] = [
    Step::WorkspacesClosed,
    Step::OperationsFinished,
    Step::WorkspacesStopped,
    Step::RoutesClosed,
    Step::ApiStopped,
    Step::BackgroundStopped,
];

/// The result of [`prepare`]: everything the asynchronous start needs from the synchronous phase.
pub struct Prepared {
    // First field: dropped last, so a failed start releases the folder after everything else.
    pub(super) lock: DataLock,
    pub(super) config: HostConfig,
    pub(super) pull: PullProxy,
    pub(super) endpoints: PuddleEndpoints,
    pub(super) roots: CorporateRoots,
    pub(super) steps: Vec<Step>,
}

impl std::fmt::Debug for Prepared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Prepared")
            .field("pull", &self.pull)
            .field("steps", &self.steps)
            .finish_non_exhaustive()
    }
}

impl Prepared {
    /// The steps run so far.
    #[must_use]
    pub fn steps(&self) -> &[Step] {
        &self.steps
    }
}

/// The synchronous phase of starting. Call it from `main` before building the async runtime or
/// starting any thread (the desktop shell: before the UI toolkit starts its own).
///
/// The order is the dependency order. The data folder is locked first (a second process on it
/// is refused with [`HostError::DataFolder`] before it touches anything). The pull proxy is bound first because the runtime's image
/// pulls need its address; the runtime is checked next; the
/// corporate roots are read last and handed to the runtime when it opens, because msb fixes them
/// then.
///
/// # Errors
///
/// [`HostError`] from the step that failed; nothing is left running.
pub fn prepare(config: HostConfig, platform: &dyn Platform) -> Result<Prepared, HostError> {
    // First, before anything is bound or looked at: a second process on this folder would
    // reconcile away the first one's running sandboxes.
    let lock = DataLock::acquire(config.paths.data())?;
    // Before the first socket; said in the log once the start has got past its checks.
    let open_files = puddle_fd_limit::raise_open_file_limit(puddle_fd_limit::Reach::Soft);
    let mut steps = vec![Step::DataFolderLocked];
    let endpoints = PuddleEndpoints::new();
    let pull = PullProxy::bind(&endpoints).map_err(HostError::PullProxy)?;
    steps.push(Step::PullProxyBound);
    platform.check_runtime(&config.layout, &config.expected_runtime)?;
    steps.push(Step::RuntimeChecked);
    let roots = platform.corporate_roots()?;
    steps.push(Step::RootsRead);
    tracing::info!(
        roots = roots.certificates().len(),
        "corporate root certificates read"
    );
    open_files.log(FDS_NEEDED);
    Ok(Prepared {
        lock,
        config,
        pull,
        endpoints,
        roots,
        steps,
    })
}

/// Descriptors the host needs for every connection the proxy lets one workspace open (its cap is
/// 4096; each costs a socket here), with room for the store, the API, routes and the runtime.
const FDS_NEEDED: u64 = 4096 + 1024;
