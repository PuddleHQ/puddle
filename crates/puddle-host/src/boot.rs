// SPDX-License-Identifier: AGPL-3.0-or-later
//! What every sandbox is built from: Puddle's boot files, the egress route and the guest's proxy
//! and trust configuration.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use puddle_agent_proto::editor::{EditorServer, ServerKind};
use puddle_api::StartFolder;
use puddle_boot::{
    BootHook, BootPlan, GitIdentity, with_boot_mounts, with_code_server_mount, write_assets,
};
use puddle_ca::{CaCertificate, TrustBundle};
use puddle_certs::devcert::GuestDevCert;
use puddle_certs::{CorporateRoots, GuestTrust};
use puddle_compute::{DiskSize, FileMount, ImageConfig, OwnedDisk, SandboxSpec, VsockRoute};
use puddle_guest_env::{
    MiseSettings, ProxySettings, guest_proxy_config, mise_config, ssh_known_hosts_file,
};
use puddle_ipc::{IpcError, IpcRoot};
use puddle_proxy::{Proxy, Route};
use puddle_settings::MiseMode;
use puddle_store::Store;
use puddle_types::{
    GuestEnv, GuestFile, GuestPath, ImageRef, MemoryMib, SessionUser, WorkspaceId, guest_path,
};
use puddle_workspace::{Layout, WORKSPACES_ROOT};

use crate::dev_cert::GuestCertSource;
use crate::git_hosts::{Authors, SshListing};
use crate::shell_init::{self, Copies};
use crate::{GuestSettings, HostError};

/// Why a sandbox's boot inputs could not be made. The messages are for the user.
#[derive(Debug, thiserror::Error)]
pub(crate) enum BootInputError {
    /// The egress route's endpoint could not be opened.
    #[error(transparent)]
    Route(#[from] IpcError),
    /// The session user's home is not a guest path.
    #[error(transparent)]
    Home(#[from] puddle_types::ValidationError),
    /// A guest file could not be rendered.
    #[error(transparent)]
    GuestFiles(#[from] puddle_guest_env::ConfigError),
    /// The boot plan is refused.
    #[error(transparent)]
    Plan(#[from] puddle_boot::PlanError),
    /// The workspace's folders could not be worked out.
    #[error(transparent)]
    Layout(#[from] puddle_workspace::WorkspaceError),
}

/// The guest vsock port the agent connects to; the route of every sandbox listens behind it.
pub(crate) const AGENT_ROUTE_PORT: u32 = 5000;

/// Folder below the guest-share root that holds the files every sandbox mounts.
const ASSET_DIR: &str = "puddle";

/// The folder below a workspace's `.puddle` that holds the editor server's user data and
/// extensions.
const EDITOR_DATA_DIR: &str = "code-server";

/// The same for Microsoft's server, which keeps its own data, its extensions and its downloaded
/// builds there (never mixed with code-server's).
const MICROSOFT_DATA_DIR: &str = "vscode-server";

/// Folder below the guest-share root with one folder per workspace for its shell init.
const SHELL_INIT_DIR: &str = "shell-init";

/// Where Docker keeps its data in the guest.
const DOCKER_DATA_DIR: &str = "/var/lib/docker";

/// How big Docker's own disk is. The file behind it is sparse, so the host only spends what the
/// guest writes; the ceiling is what Docker can fill before it reports a full disk.
const DOCKER_DISK_SIZE: DiskSize = DiskSize::gib(20);

/// How big a new workspace's root disk is. The image file is sparse (NTFS sparse flag on Windows,
/// holes on Linux), so a fresh disk costs about what a 4 GiB one did and the host spends more only
/// as the guest writes; the size is the ceiling for packages, tool caches and `/home`. A workspace
/// made before this size existed keeps the 4 GiB it was made with.
const WORKSPACE_ROOT_DISK_SIZE: DiskSize = DiskSize::gib(16);

/// The root disk a workspace's sandbox is made with: the default, or the size the user grew it to
/// when that is more, so a reset or a made-again sandbox keeps what was grown.
pub(crate) fn root_disk_size(grown_mib: Option<u32>) -> DiskSize {
    grown_mib.map_or(WORKSPACE_ROOT_DISK_SIZE, |mib| {
        WORKSPACE_ROOT_DISK_SIZE.max(DiskSize::mib(mib))
    })
}

/// The disk Docker's data lives on: one per sandbox, deleted with it.
fn docker_disk() -> OwnedDisk {
    OwnedDisk {
        guest: guest_path!(DOCKER_DATA_DIR),
        size: DOCKER_DISK_SIZE,
    }
}

/// The pieces shared by every sandbox of this host.
pub(crate) struct BootKit {
    proxy: Arc<Proxy>,
    ipc: IpcRoot,
    hook: BootHook,
    assets: Vec<FileMount>,
    agent: PathBuf,
    /// The code-server release as the guests mount it (inside the guest share), when bundled.
    code_server: Option<PathBuf>,
    roots: CorporateRoots,
    git: Option<GitIdentity>,
    shell_init: Copies,
    dev_cert: Option<Arc<dyn GuestCertSource>>,
}

/// Where the agent listens for the guest's SSH clients when the workspace uses the user's SSH
/// agent for a host.
const GUEST_SSH_AGENT_SOCKET: &str = "/run/puddle/ssh-agent.sock";

/// How long a start waits for the development certificate: the system's one-time confirmation
/// may be open on the user's desktop. After that the workspace starts without it (and gets it at
/// its next start).
const DEV_CERT_WAIT: std::time::Duration = std::time::Duration::from_secs(120);

/// What differs from one sandbox to the next, besides the image: the CA the sandbox's guest
/// trusts, who commits, and the user's own variables.
pub(crate) struct GuestInputs<'a> {
    /// This start's CA certificate (public), added to the guest's trust bundle.
    pub(crate) ca: &'a CaCertificate,
    /// The commit authors: the fallback and the rules by remote.
    pub(crate) authors: &'a Authors,
    /// This start's SSH host keys and the names they are listed under, when Git over SSH is on.
    pub(crate) ssh: Option<&'a SshListing>,
    /// The user's variables for this workspace, a secret as its stand-in. They sit under Puddle's
    /// own (the proxy and trust variables win), except that a `JAVA_TOOL_OPTIONS`, `MAVEN_ARGS`,
    /// `GRADLE_USER_HOME` or `DOCKER_CONFIG` of the user's is kept and Puddle's setting is added to
    /// it, as it is to the image's.
    pub(crate) env: &'a GuestEnv,
    /// The browser editor's server for this workspace, when the host bundles one.
    pub(crate) editor: Option<&'a EditorServer>,
    /// The development certificate and its key, when the host has one for this start.
    pub(crate) dev_cert: Option<&'a GuestDevCert>,
    /// The volume's mount point.
    pub(crate) root: &'a GuestPath,
    /// Where login shells land; `None` is the volume root.
    pub(crate) start_folder: Option<&'a StartFolder>,
    /// The multi-root file for the editor, when the workspace has two or more checkouts.
    pub(crate) editor_file: Option<&'a GuestFile>,
    /// The guest user sessions run as: the boot hook adds it, and per-user settings go to its
    /// home.
    pub(crate) session_user: SessionUser,
    /// Whether the guest gets its SSH agent socket, which carries each client to the user's agent
    /// on the host (for the hosts the workspace has it on for).
    pub(crate) ssh_agent: bool,
    /// The managed mise layer, when it is on for this workspace.
    pub(crate) mise: Option<&'a MiseLayer>,
}

/// Where the managed mise layer lives in one workspace and what it may trust.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MiseLayer {
    /// `/workspaces/<id>/.puddle/mise`.
    pub(crate) dir: GuestPath,
    /// The workspace's volume when the repository's own code is trusted (`repo_code`).
    pub(crate) trusted: Option<GuestPath>,
}

impl MiseLayer {
    /// The layer of workspace `id` for `mode`; `None` while the layer is off.
    pub(crate) fn of(id: &WorkspaceId, mode: MiseMode) -> Result<Option<Self>, BootInputError> {
        let MiseMode::On { trust } = mode else {
            return Ok(None);
        };
        let layout = Layout::new(id)?;
        Ok(Some(Self {
            dir: layout.mise_dir()?,
            trusted: trust.then(|| layout.mount().clone()),
        }))
    }
}

impl BootKit {
    /// Writes the boot files and a copy of the agent below `guest_share` (the only place msb
    /// mounts from) and gathers the rest.
    pub(crate) fn new(
        guest_share: &Path,
        guest: &GuestSettings,
        proxy: Arc<Proxy>,
        roots: &CorporateRoots,
        store: Arc<Store>,
        state_dir: &Path,
    ) -> Result<Self, HostError> {
        let io = |what: &str, e: std::io::Error| HostError::GuestFiles(format!("{what}: {e}"));
        if !guest.agent_binary.is_file() {
            return Err(HostError::AgentMissing {
                path: guest.agent_binary.clone(),
            });
        }
        let dir = guest_share.join(ASSET_DIR);
        let assets = write_assets(&dir).map_err(|e| io("write the boot files", e))?;
        let agent = dir.join("puddle-agent");
        // Copy beside the target and rename over it, so a sandbox booting from the old file
        // never sees half of the new one.
        let partial = dir.join("puddle-agent.partial");
        std::fs::copy(&guest.agent_binary, &partial).map_err(|e| io("copy the agent", e))?;
        std::fs::rename(&partial, &agent).map_err(|e| io("install the agent", e))?;
        let code_server = guest
            .code_server
            .as_deref()
            .map(|release| install_code_server(release, &dir))
            .transpose()
            .map_err(|e| HostError::GuestFiles(format!("install code-server: {e}")))?;
        // Stable routes keep every sandbox across a restart of this process. Without them (the
        // runtime folder is unusable) the routes are random again, and a restart rebuilds sandboxes.
        let ipc = match IpcRoot::stable(state_dir) {
            Ok(root) => root,
            Err(e) => {
                tracing::warn!(error = %e, "no stable sandbox routes: a restart of Puddle will make every sandbox again");
                IpcRoot::new().map_err(|e| HostError::GuestFiles(e.to_string()))?
            }
        };
        Ok(Self {
            proxy,
            ipc,
            hook: BootHook::new().with_timeout(guest.boot_timeout),
            assets,
            agent,
            code_server,
            roots: roots.clone(),
            git: guest.git_identity.clone(),
            shell_init: Copies::new(guest_share.join(SHELL_INIT_DIR), store),
            dev_cert: None,
        })
    }

    /// The folders that carry each workspace's shell init to its guest.
    pub(crate) fn shell_init(&self) -> &Copies {
        &self.shell_init
    }

    /// The same kit whose sandboxes are given the development certificate `source` makes.
    pub(crate) fn with_dev_cert(mut self, source: Arc<dyn GuestCertSource>) -> Self {
        self.dev_cert = Some(source);
        self
    }

    /// The development certificate for a start, read now so a renewed one reaches the guest at
    /// its next boot. `None` without a source, when it cannot be made (logged) or when the
    /// system's confirmation stays unanswered for [`DEV_CERT_WAIT`].
    pub(crate) async fn dev_cert(&self) -> Option<GuestDevCert> {
        crate::dev_cert::certificate_within(self.dev_cert.clone()?, DEV_CERT_WAIT).await
    }

    pub(crate) fn hook(&self) -> &BootHook {
        &self.hook
    }

    /// Starts the egress route of `workspace`: an owner-only endpoint served by the shared proxy,
    /// the same one at every run of Puddle, so a sandbox made by an earlier run finds its route
    /// where its spec says. A name that cannot be had (taken by another process, no stable root)
    /// gets a random endpoint instead: that route is not the one an older sandbox has, so such a
    /// sandbox is made again. It lives as long as the returned [`Route`].
    pub(crate) fn route(&self, workspace: &WorkspaceId) -> Result<Route, BootInputError> {
        let listener = match self.ipc.listen_named(workspace.as_str()) {
            Ok(listener) => listener,
            Err(e) => {
                tracing::warn!(%workspace, error = %e, "the workspace's stable route is not available, so it gets a random one");
                self.ipc.listen()?
            }
        };
        Ok(self.proxy.serve_route(listener, workspace.clone()))
    }

    /// Whether a sandbox made with `routes` starts with `route`: its agent route is this endpoint.
    pub(crate) fn serves(routes: &[VsockRoute], route: &Route) -> bool {
        routes
            .iter()
            .any(|r| r.guest_port == AGENT_ROUTE_PORT && r.host == route.endpoint().path())
    }

    /// The boot plan and the environment for a sandbox of `image`. The same inputs give the same
    /// plan, so running it again in the running guest changes only what `guest` changed.
    pub(crate) fn plan(
        &self,
        image: &ImageConfig,
        guest: &GuestInputs<'_>,
    ) -> Result<(BootPlan, GuestEnv), BootInputError> {
        // The user's variables count as the image's here, so the few that Puddle adds to (not
        // replaces) keep the user's value and the files that follow `GRADLE_USER_HOME` and
        // `DOCKER_CONFIG` land where the user pointed them.
        let mut image_env = image.env.clone();
        image_env.extend(guest.env.iter().map(|(k, v)| (k.to_owned(), v.to_owned())));
        let user = guest.session_user;
        let ssh_agent = guest
            .ssh_agent
            .then(|| GuestPath::new(GUEST_SSH_AGENT_SOCKET))
            .transpose()?;
        let settings = ProxySettings {
            home: GuestPath::new(user.home())?,
            ssh_agent: ssh_agent.clone(),
            ..ProxySettings::default()
        };
        let proxy = guest_proxy_config(&settings, &image_env)?;
        let mut trust =
            GuestTrust::new(&self.roots, &TrustBundle::new().with(guest.ca.clone())).for_user(user);
        if let Some(dev) = guest.dev_cert {
            trust = trust.with_dev_cert(dev.clone());
        }
        let mut env = guest.env.clone();
        env.extend(&proxy.env);
        env.extend(&trust.env());
        let mise = guest
            .mise
            .map(|layer| {
                mise_config(&MiseSettings {
                    dir: layer.dir.clone(),
                    ceiling: GuestPath::new(WORKSPACES_ROOT)?,
                    trusted: layer.trusted.clone(),
                    image_path: image_env
                        .iter()
                        .rev()
                        .find(|(name, _)| name == "PATH")
                        .map(|(_, value)| value.clone()),
                })
                .map_err(BootInputError::from)
            })
            .transpose()?;
        if let Some(mise) = &mise {
            env.extend(&mise.env);
        }
        let mut builder = BootPlan::builder(image)
            .session_user(user)
            .env(&env)
            .files(proxy.files)
            .files(trust.guest_files())
            .files(mise.map(|m| m.files).unwrap_or_default())
            .file(shell_init::loader())
            .file(puddle_boot::start_folder_file(
                guest.root,
                &crate::editor_file::start_path(guest.root, guest.start_folder),
            ));
        if let Some(file) = guest.editor_file {
            builder = builder.file(file.clone());
        }
        if let Some(lines) = guest.ssh.and_then(SshListing::lines) {
            builder = builder.file(ssh_known_hosts_file(&lines)?);
        }
        for step in trust.boot_steps() {
            builder = builder.step(step);
        }
        // The first identity's author, else the one the host was configured with.
        if let Some(author) = guest.authors.fallback.as_ref().or(self.git.as_ref()) {
            builder = builder.git_identity(author.clone());
        }
        for rule in &guest.authors.rules {
            builder = builder.git_author_rule(rule.clone());
        }
        if let Some(editor) = guest.editor {
            for (name, value) in editor.env() {
                builder = builder.agent_env(name, value);
            }
        }
        if let Some(socket) = &ssh_agent {
            builder = builder
                .agent_env("PUDDLE_AGENT_SSH_AGENT", socket.as_str())
                .agent_env("PUDDLE_AGENT_SSH_AGENT_USER", user.name());
        }
        let plan = builder.build()?;
        Ok((plan, env))
    }

    /// The spec of a new sandbox: image, memory, environment, the route, Puddle's mounts and the
    /// disk of its own that Docker's data lives on, so the layers never sit on the guest's overlay
    /// root nor on the workspace volume.
    pub(crate) fn spec(
        &self,
        name: &WorkspaceId,
        image: ImageRef,
        memory: MemoryMib,
        env: &GuestEnv,
        route: &Route,
    ) -> SandboxSpec {
        let spec = SandboxSpec::new(name.sandbox_name(), image)
            .with_memory(memory)
            .with_env(env)
            .with_route(VsockRoute::new(AGENT_ROUTE_PORT, route.endpoint().path()))
            .with_owned_disk(docker_disk());
        let spec = with_boot_mounts(spec, self.assets.clone(), Some(&self.agent));
        match &self.code_server {
            Some(release) => with_code_server_mount(spec, release),
            None => spec,
        }
    }

    /// How the browser editor's server `kind` runs in workspace `id`: its data on the volume, the
    /// volume as the folder it opens, or the multi-root file once an attach wrote one. `None` for
    /// code-server when this host bundles none; Microsoft's server needs nothing bundled (the
    /// guest downloads it).
    pub(crate) fn editor(
        &self,
        id: &WorkspaceId,
        grace_secs: u32,
        kind: ServerKind,
    ) -> Result<Option<EditorServer>, BootInputError> {
        if self.code_server.is_none() && kind == ServerKind::CodeServer {
            return Ok(None);
        }
        let layout = Layout::new(id)?;
        let data = layout.puddle_dir()?;
        let mount = layout.mount().to_string();
        let mut server = match kind {
            ServerKind::CodeServer => {
                EditorServer::new(format!("{data}/{EDITOR_DATA_DIR}"), mount, grace_secs)
            }
            ServerKind::Microsoft {
                telemetry,
                auto_update,
            } => EditorServer::microsoft(
                format!("{data}/{MICROSOFT_DATA_DIR}"),
                mount,
                grace_secs,
                telemetry,
                auto_update,
            ),
        };
        server.workspace_file = crate::editor_file::workspace_file_path(layout.mount(), id)
            .map(|file| file.as_str().to_owned());
        Ok(Some(server))
    }

    pub(crate) fn ipc(&self) -> &IpcRoot {
        &self.ipc
    }
}

/// The folder below the guest share that holds the code-server release the guests mount. It has
/// one name for every release: a sandbox keeps the mount source it was made with, so a name that
/// changed with the version would make every sandbox of an earlier run unstartable after an
/// update (and a restart that keeps sandboxes would have to make them again).
const CODE_SERVER_DIR: &str = "code-server";

/// Puts the code-server release at `release` in [`CODE_SERVER_DIR`] below `dir` (inside the guest
/// share, the only place msb mounts from) and returns where. A folder that is complete for this
/// release is reused; another release replaces it in place, which is safe because this runs when
/// the host starts, before any sandbox of this run exists (one host runs per data folder, and the
/// sandboxes of an earlier run are stopped). Files are hard-linked where the folders share a disk,
/// else copied.
fn install_code_server(release: &Path, dir: &Path) -> std::io::Result<PathBuf> {
    let manifest = std::fs::read_to_string(release.join("code-server.json"))?;
    serde_json::from_str::<serde_json::Value>(&manifest)
        .ok()
        .and_then(|m| m.get("version")?.as_str().map(ToOwned::to_owned))
        .filter(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit() || b == b'.'))
        .ok_or_else(|| {
            std::io::Error::other(format!(
                "{} has no usable version",
                release.join("code-server.json").display()
            ))
        })?;
    let target = dir.join(CODE_SERVER_DIR);
    remove_legacy_releases(dir);
    let done = target.join(INSTALLED_MARKER);
    if std::fs::read_to_string(&done).is_ok_and(|installed| installed == manifest) {
        return Ok(target);
    }
    let partial = dir.join(format!("{CODE_SERVER_DIR}.partial"));
    if partial.exists() {
        std::fs::remove_dir_all(&partial)?;
    }
    tracing::info!(from = %release.display(), to = %target.display(), "installing code-server for the guests");
    link_or_copy_tree(release, &partial, &|from, to| std::fs::hard_link(from, to))?;
    std::fs::write(partial.join(INSTALLED_MARKER), &manifest)?;
    if target.exists() {
        std::fs::remove_dir_all(&target)?;
    }
    std::fs::rename(&partial, &target)?;
    Ok(target)
}

/// Removes what earlier versions of Puddle left below `dir`: the `code-server-<version>` folders
/// (a sandbox of that time is made again at its next start, since its route is not a stable one
/// either). Failing to remove one only leaves it for the next start.
fn remove_legacy_releases(dir: &Path) {
    let legacy: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("code-server-"))
        .map(|e| e.path())
        .collect();
    for path in legacy {
        let gone = if path.is_dir() {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        if let Err(err) = gone {
            tracing::warn!(path = %path.display(), %err, "an old code-server release could not be removed");
        }
    }
}

/// Written last: a folder with it is complete.
const INSTALLED_MARKER: &str = ".puddle-installed";

fn link_or_copy_tree(
    from: &Path,
    to: &Path,
    link: &dyn Fn(&Path, &Path) -> std::io::Result<()>,
) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let (src, dst) = (entry.path(), to.join(entry.file_name()));
        if entry.file_type()?.is_dir() {
            link_or_copy_tree(&src, &dst, link)?;
        } else if link(&src, &dst).is_err() {
            std::fs::copy(&src, &dst)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
