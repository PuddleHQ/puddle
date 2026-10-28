// SPDX-License-Identifier: GPL-3.0-or-later
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
use puddle_compute::{FileMount, ImageConfig, SandboxSpec, VsockRoute};
use puddle_guest_env::{ProxySettings, guest_proxy_config};
use puddle_ipc::IpcRoot;
use puddle_proxy::{Proxy, Route};
use puddle_store::Store;
use puddle_types::{
    GuestEnv, GuestFile, GuestPath, ImageRef, MemoryMib, WorkspaceId, WorkspaceName,
};
use puddle_workspace::Layout;

use crate::dev_cert::GuestCertSource;
use crate::git_hosts::Authors;
use crate::shell_init::{self, Copies};
use crate::{GuestSettings, HostError};

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
        let ipc = IpcRoot::new().map_err(|e| HostError::GuestFiles(e.to_string()))?;
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

    /// Starts the egress route of `sandbox`: a fresh owner-only endpoint served by the shared
    /// proxy. It lives as long as the returned [`Route`].
    pub(crate) fn route(&self, workspace: &WorkspaceName) -> Result<Route, String> {
        let listener = self.ipc.listen().map_err(|e| e.to_string())?;
        Ok(self.proxy.serve_route(listener, workspace.clone()))
    }

    /// The boot plan and the environment for a sandbox of `image`. The same inputs give the same
    /// plan, so running it again in the running guest changes only what `guest` changed.
    pub(crate) fn plan(
        &self,
        image: &ImageConfig,
        guest: &GuestInputs<'_>,
    ) -> Result<(BootPlan, GuestEnv), String> {
        // The user's variables count as the image's here, so the few that Puddle adds to (not
        // replaces) keep the user's value and the files that follow `GRADLE_USER_HOME` and
        // `DOCKER_CONFIG` land where the user pointed them.
        let mut image_env = image.env.clone();
        image_env.extend(guest.env.iter().map(|(k, v)| (k.to_owned(), v.to_owned())));
        let proxy =
            guest_proxy_config(&ProxySettings::default(), &image_env).map_err(|e| e.to_string())?;
        let mut trust = GuestTrust::new(&self.roots, &TrustBundle::new().with(guest.ca.clone()));
        if let Some(dev) = guest.dev_cert {
            trust = trust.with_dev_cert(dev.clone());
        }
        let mut env = guest.env.clone();
        env.extend(&proxy.env);
        env.extend(&trust.env());
        let mut builder = BootPlan::builder(image)
            .env(&env)
            .files(proxy.files)
            .files(trust.guest_files())
            .file(shell_init::loader())
            .file(puddle_boot::start_folder_file(
                guest.root,
                &crate::editor_file::start_path(guest.root, guest.start_folder),
            ));
        if let Some(file) = guest.editor_file {
            builder = builder.file(file.clone());
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
        let plan = builder.build().map_err(|e| e.to_string())?;
        Ok((plan, env))
    }

    /// The spec of a new sandbox: image, memory, environment, the route and Puddle's mounts.
    pub(crate) fn spec(
        &self,
        name: &WorkspaceName,
        image: ImageRef,
        memory: MemoryMib,
        env: &GuestEnv,
        route: &Route,
    ) -> SandboxSpec {
        let spec = SandboxSpec::new(name.sandbox_name(), image)
            .with_memory(memory)
            .with_env(env)
            .with_route(VsockRoute::new(AGENT_ROUTE_PORT, route.endpoint().path()));
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
    ) -> Result<Option<EditorServer>, String> {
        if self.code_server.is_none() && kind == ServerKind::CodeServer {
            return Ok(None);
        }
        let layout = Layout::new(id).map_err(|e| e.to_string())?;
        let data = layout.puddle_dir().map_err(|e| e.to_string())?;
        let mount = layout.mount().to_string();
        let mut server = match kind {
            ServerKind::CodeServer => {
                EditorServer::new(format!("{data}/{EDITOR_DATA_DIR}"), mount, grace_secs)
            }
            ServerKind::Microsoft { telemetry } => EditorServer::microsoft(
                format!("{data}/{MICROSOFT_DATA_DIR}"),
                mount,
                grace_secs,
                telemetry,
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

/// Puts the code-server release at `release` below `dir` (inside the guest share, the only place
/// msb mounts from) and returns where. The folder is named for the release's version, so two
/// versions never mix and a sandbox that still mounts the old one keeps working; a folder that
/// is already complete is reused. Files are hard-linked where the folders share a disk, else
/// copied.
fn install_code_server(release: &Path, dir: &Path) -> std::io::Result<PathBuf> {
    let manifest = std::fs::read_to_string(release.join("code-server.json"))?;
    let version = serde_json::from_str::<serde_json::Value>(&manifest)
        .ok()
        .and_then(|m| m.get("version")?.as_str().map(ToOwned::to_owned))
        .filter(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit() || b == b'.'))
        .ok_or_else(|| {
            std::io::Error::other(format!(
                "{} has no usable version",
                release.join("code-server.json").display()
            ))
        })?;
    let target = dir.join(format!("code-server-{version}"));
    let done = target.join(INSTALLED_MARKER);
    if done.is_file() {
        return Ok(target);
    }
    let partial = dir.join(format!("code-server-{version}.partial"));
    if partial.exists() {
        std::fs::remove_dir_all(&partial)?;
    }
    if target.exists() {
        std::fs::remove_dir_all(&target)?;
    }
    tracing::info!(from = %release.display(), to = %target.display(), "installing code-server for the guests");
    link_or_copy_tree(release, &partial, &|from, to| std::fs::hard_link(from, to))?;
    std::fs::write(partial.join(INSTALLED_MARKER), &manifest)?;
    std::fs::rename(&partial, &target)?;
    Ok(target)
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
mod tests {
    use super::*;

    fn release(dir: &Path, version: &str) -> PathBuf {
        let release = dir.join("release");
        std::fs::create_dir_all(release.join("bin")).unwrap();
        std::fs::write(release.join("bin/code-server"), "#!/bin/sh\n").unwrap();
        std::fs::write(
            release.join("code-server.json"),
            format!("{{\"version\": \"{version}\"}}"),
        )
        .unwrap();
        release
    }

    #[test]
    fn the_release_is_installed_under_its_version_and_reused_when_complete() {
        let tmp = tempfile::tempdir().unwrap();
        let share = tmp.path().join("share");
        std::fs::create_dir(&share).unwrap();
        let release = release(tmp.path(), "4.1.0");
        let target = install_code_server(&release, &share).unwrap();
        assert_eq!(target, share.join("code-server-4.1.0"));
        assert!(target.join("bin/code-server").is_file());
        assert!(target.join(INSTALLED_MARKER).is_file());
        // A second start finds it complete and leaves it alone.
        std::fs::write(target.join("sentinel"), "x").unwrap();
        assert_eq!(install_code_server(&release, &share).unwrap(), target);
        assert!(target.join("sentinel").is_file());
        // Another version sits beside it.
        let newer = release.with_file_name("newer");
        std::fs::create_dir(&newer).unwrap();
        std::fs::write(newer.join("code-server.json"), r#"{"version":"4.2.0"}"#).unwrap();
        assert_eq!(
            install_code_server(&newer, &share).unwrap(),
            share.join("code-server-4.2.0")
        );
        assert!(target.is_dir());
    }

    #[test]
    fn a_half_installed_folder_is_made_again() {
        let tmp = tempfile::tempdir().unwrap();
        let share = tmp.path().join("share");
        std::fs::create_dir_all(share.join("code-server-4.1.0.partial")).unwrap();
        std::fs::create_dir_all(share.join("code-server-4.1.0")).unwrap();
        std::fs::write(share.join("code-server-4.1.0/stale"), "x").unwrap();
        let target = install_code_server(&release(tmp.path(), "4.1.0"), &share).unwrap();
        assert!(!target.join("stale").exists());
        assert!(target.join(INSTALLED_MARKER).is_file());
        assert!(!share.join("code-server-4.1.0.partial").exists());
    }

    #[test]
    fn a_release_without_a_usable_version_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let share = tmp.path().join("share");
        std::fs::create_dir(&share).unwrap();
        let empty = tmp.path().join("empty");
        std::fs::create_dir(&empty).unwrap();
        assert!(install_code_server(&empty, &share).is_err());
        for bad in [
            "not json",
            r#"{"version": "../x"}"#,
            r#"{"version": ""}"#,
            "{}",
        ] {
            std::fs::write(empty.join("code-server.json"), bad).unwrap();
            let err = install_code_server(&empty, &share).unwrap_err().to_string();
            assert!(err.contains("no usable version"), "{bad}: {err}");
        }
    }

    #[test]
    fn files_are_copied_where_a_link_is_not_possible() {
        let tmp = tempfile::tempdir().unwrap();
        let from = release(tmp.path(), "4.1.0");
        let to = tmp.path().join("copy");
        let no_links = |_: &Path, _: &Path| Err(std::io::Error::other("different disks"));
        link_or_copy_tree(&from, &to, &no_links).unwrap();
        assert_eq!(
            std::fs::read_to_string(to.join("bin/code-server")).unwrap(),
            "#!/bin/sh\n"
        );
    }

    #[test]
    fn the_guest_mount_is_the_path_the_agent_is_told() {
        assert_eq!(
            puddle_boot::CODE_SERVER_GUEST,
            puddle_agent_proto::editor::GUEST_DIR
        );
    }
}
