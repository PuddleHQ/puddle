// SPDX-License-Identifier: GPL-3.0-or-later
//! How a workspace is reached (desktop, browser, SSH) and how it is quiesced.

use std::sync::Arc;

use puddle_agent_proto::editor::ServerKind;
use puddle_api::{
    Attached, BrowserTarget, DesktopTarget, EditorLogin, EditorSignIn, OpenTarget, WorkspaceRecord,
};
use puddle_boot::GatedSandbox;
use puddle_compute::{ComputeError, Runtime};
use puddle_settings::{Effective, ReconnectionGrace, ServerChoice, resolve};
use puddle_ssh::SshEndpoint;
use puddle_types::{Problem, SessionUser, WorkspaceId};

use crate::ssh_config::ssh_host;
use crate::ssh_target::{ReportingTarget, report_no_endpoint};

use super::{GuestLogin, GuestState, HostWorkspaces, problem_key, with_leftovers};
use crate::git_hosts::GitProblem;
use crate::kubernetes::kubernetes_key;
use crate::merge_notice::merge_notice_prefix;

impl<R: Runtime + Clone> HostWorkspaces<R> {
    /// The single gate for direct SSH: whether the user allowed it for `name`. Whatever opens an
    /// SSH way into a workspace (the endpoint, and any ssh config entry) must check this first.
    /// It is its override over the global default over off. Settings that cannot be read count as
    /// off, with the reason logged (and shown on the workspace by the API).
    #[must_use]
    pub fn direct_ssh_allowed(&self, name: &WorkspaceId) -> bool {
        match self.direct_ssh_setting(name) {
            Ok(on) => on,
            Err(reason) => {
                tracing::warn!(workspace = %name, %reason, "direct SSH stays off: the settings cannot be read");
                false
            }
        }
    }

    /// What a desktop editor opens in the running workspace `record`, once its ssh entry is in
    /// place: the multi-root file when the editor opens one, else the start folder or the volume
    /// root. Asked again, it writes the entry again, so an entry that could not be written when the
    /// endpoint opened (a config file that was locked) gets another try now, and says why if it
    /// fails again.
    ///
    /// # Errors
    ///
    /// Why the editor cannot be opened, for the user.
    pub(super) async fn desktop_target(
        &self,
        record: &WorkspaceRecord,
    ) -> Result<DesktopTarget, String> {
        let name = &record.id;
        match self.direct_ssh_setting(name) {
            Ok(true) => {}
            Ok(false) => {
                return Err("direct SSH is off for this workspace; allow it first".to_owned());
            }
            Err(reason) => {
                tracing::warn!(workspace = %name, %reason, "direct SSH stays off: the settings cannot be read");
                return Err(format!(
                    "the settings cannot be read ({reason}); direct SSH counts as off until they can"
                ));
            }
        }
        let live = self.inner.live.lock().await;
        let entry = live.get(name);
        let Some(endpoint) = entry
            .and_then(|e| e.ssh.as_ref())
            .map(|ssh| ssh.endpoint().path().to_path_buf())
        else {
            return Err(format!(
                "direct SSH has no endpoint in {name}; restart the workspace to try again"
            ));
        };
        let guest = entry.and_then(|e| e.guest.as_ref());
        let user = guest.map_or(SessionUser::Root, |g| g.session_user);
        let open = match guest {
            Some(guest) => Self::open_target(record, guest),
            None => return Err(format!("{name} is not running; start it first")),
        };
        let key = problem_key("ssh", name);
        self.inner
            .ssh
            .publish(name, &endpoint, user)
            .map_err(|reason| {
                self.ssh_not_usable(name, &reason);
                format!("direct SSH is on but the ssh config could not be written: {reason}")
            })?;
        self.inner.problems.clear(&key);
        Ok(DesktopTarget {
            ssh_host: ssh_host(name),
            path: open.path,
            is_workspace_file: open.is_workspace_file,
        })
    }

    /// What an editor opens in the running `guest`: the multi-root file when the editor opens one,
    /// else the start folder or the volume root. The desktop and the browser attach share it.
    pub(super) fn open_target(record: &WorkspaceRecord, guest: &GuestState) -> OpenTarget {
        match &guest.editor_file {
            Some(file) => OpenTarget {
                path: file.path().as_str().to_owned(),
                is_workspace_file: true,
            },
            None => OpenTarget {
                path: crate::editor_file::start_path(&guest.root, record.start_folder.as_ref())
                    .as_str()
                    .to_owned(),
                is_workspace_file: false,
            },
        }
    }

    /// Which server the browser editor runs at this start: Microsoft's only when the user chose
    /// it and the recorded consent covers today's terms; anything else, settings that cannot be
    /// read included, is the bundled code-server (a consent is never assumed).
    pub(super) fn editor_kind(&self) -> ServerKind {
        match crate::settings_read::global(self.inner.settings.as_ref()) {
            Ok(global) => match global.editor_server() {
                ServerChoice::Microsoft => ServerKind::Microsoft {
                    telemetry: global.vscode_server.telemetry(),
                    auto_update: global.vscode_server.auto_update(),
                },
                ServerChoice::CodeServer => {
                    if global.vscode_server.server() == ServerChoice::Microsoft {
                        tracing::warn!(
                            "the editor runs code-server: Microsoft's server is chosen but the consent no longer covers its terms"
                        );
                    }
                    ServerKind::CodeServer
                }
            },
            Err(reason) => {
                tracing::warn!(%reason, "the editor runs code-server: the settings cannot be read");
                ServerKind::CodeServer
            }
        }
    }

    /// The browser editor's reconnection grace for `name`, in seconds: the settings' value, the
    /// default when they cannot be read (the editor is not worth a failed start).
    pub(super) fn reconnection_grace(&self, name: &WorkspaceId) -> u32 {
        let settings = self.inner.settings.as_ref();
        let read = || {
            let global = crate::settings_read::global(settings)?;
            let own = crate::settings_read::workspace(settings, name)?;
            Ok::<_, String>(resolve(&global, Some(&own)).reconnection_grace.value)
        };
        match read() {
            Ok(grace) => grace.secs(),
            Err(reason) => {
                tracing::warn!(workspace = %name, %reason, "the editor keeps the default reconnection grace: the settings cannot be read");
                ReconnectionGrace::DEFAULT.secs()
            }
        }
    }

    /// Asks the launcher to open `record`'s editor in a window of its own. A launcher with no
    /// window (the default, so `puddle serve`) says the editor opens from the desktop app: the
    /// password is held back from anything but a shell.
    pub(super) async fn attach_browser(&self, record: &WorkspaceRecord) -> Attached {
        let name = &record.id;
        // No window opens on settings that cannot be read: the user may have tightened what the
        // window does (no clipboard prompt, no zoom keys), and the defaults would undo that.
        let settings = match self.window_settings(name) {
            Ok(settings) => settings,
            Err(reason) => {
                tracing::warn!(workspace = %name, %reason, "no window opens: the settings cannot be read");
                return Attached::not_opened(format!(
                    "the editor window did not open because of this: {reason}; fix the settings file, or delete it to go back to the defaults, then open the editor again"
                ));
            }
        };
        // Read under the live lock, released before the launcher is asked.
        let open = {
            let live = self.inner.live.lock().await;
            live.get(name)
                .and_then(|e| e.guest.as_ref())
                .map(|guest| Self::open_target(record, guest))
        };
        let target = BrowserTarget {
            origin: puddle_front::origin_of(self.inner.front_addr.port(), name),
            front: self.inner.front_addr,
            settings,
            login: self.inner.flavours.of(name).map(|flavour| {
                let login: Arc<dyn EditorLogin> = Arc::new(GuestLogin {
                    guests: Arc::clone(&self.inner.front_guests),
                    ports: self.inner.ports.clone(),
                    workspace: name.clone(),
                });
                // Microsoft's server takes the password file as its connection token;
                // code-server takes it in its login form.
                if flavour == puddle_front::Flavour::Microsoft {
                    EditorSignIn::ConnectionToken(login)
                } else {
                    EditorSignIn::Form(login)
                }
            }),
            open,
        };
        match self.inner.launcher.open_browser(record, &target).await {
            Ok(()) => Attached::opened(),
            Err(err) => Attached::not_opened(err.to_string()),
        }
    }

    /// What `name`'s settings say about its window, or why they cannot be read.
    pub(super) fn window_settings(&self, name: &WorkspaceId) -> Result<Effective, String> {
        let settings = self.inner.settings.as_ref();
        let global = crate::settings_read::global(settings)?;
        let own = crate::settings_read::workspace(settings, name)?;
        Ok(resolve(&global, Some(&own)))
    }

    pub(super) fn direct_ssh_setting(&self, name: &WorkspaceId) -> Result<bool, String> {
        let settings = self.inner.settings.as_ref();
        let global = crate::settings_read::global(settings)?;
        let own = crate::settings_read::workspace(settings, name)?;
        Ok(resolve(&global, Some(&own)).direct_ssh.value)
    }

    /// Opens the SSH endpoint of a running sandbox and puts its entry in Puddle's ssh config. A
    /// failure is reported to the user ([`Event::WorkspaceSshFailed`] and a problem that lasts
    /// until it works): the workspace runs without direct SSH.
    pub(super) async fn open_ssh(&self, name: &WorkspaceId, gated: Arc<GatedSandbox<R::Sandbox>>) {
        let inner = &self.inner;
        let listener = match inner.kit.ipc().listen() {
            Ok(listener) => listener,
            Err(e) => {
                self.endpoint_failed(name, &e);
                return;
            }
        };
        let target = Arc::new(ReportingTarget::new(
            gated,
            name.clone(),
            Arc::clone(&inner.events),
        ));
        let ssh = match SshEndpoint::start(listener, target) {
            Ok(ssh) => ssh,
            Err(e) => {
                self.endpoint_failed(name, &e);
                return;
            }
        };
        let endpoint = ssh.endpoint().path().to_path_buf();
        // What is left over: an endpoint this one replaced, or this one when it has no place.
        let leftover = {
            let mut live = inner.live.lock().await;
            // The switch may have gone off while the endpoint was starting: then it has no place.
            match live.get_mut(name).filter(|_| self.direct_ssh_allowed(name)) {
                Some(entry) => {
                    let stale = entry.ssh.replace(ssh);
                    let user = entry
                        .guest
                        .as_ref()
                        .map_or(SessionUser::Root, |g| g.session_user);
                    // The entry is written while the lock is held, so a switch turned off
                    // meanwhile (which takes the endpoint under the same lock) removes it
                    // after, never before.
                    self.publish_entry(name, &endpoint, user);
                    stale
                }
                None => Some(ssh),
            }
        };
        if let Some(leftover) = leftover {
            leftover.close().await;
        }
    }

    /// Puts `name`'s entry in Puddle's ssh config, naming `endpoint` and logging in as `user`. The
    /// caller has checked [`Self::direct_ssh_allowed`]. A failure is a problem on the workspace:
    /// its endpoint is up, but nothing can name it.
    pub(super) fn publish_entry(
        &self,
        name: &WorkspaceId,
        endpoint: &std::path::Path,
        user: SessionUser,
    ) {
        let key = problem_key("ssh", name);
        match self.inner.ssh.publish(name, endpoint, user) {
            Ok(()) => self.inner.problems.clear(&key),
            Err(reason) => self.ssh_not_usable(name, &reason),
        }
    }

    pub(super) fn endpoint_failed(&self, name: &WorkspaceId, error: &dyn std::fmt::Display) {
        report_no_endpoint(self.inner.events.as_ref(), name, error);
        self.ssh_not_usable(name, &format!("it has no SSH endpoint ({error})"));
    }

    /// Raises the problem that direct SSH is on for `name` and cannot be used.
    pub(super) fn ssh_not_usable(&self, name: &WorkspaceId, reason: &str) {
        tracing::warn!(workspace = %name, %reason, "direct SSH is on but not usable");
        self.inner.problems.raise(
            Problem::new(
                problem_key("ssh", name),
                format!(
                    "Direct SSH does not work in {name}",
                    name = self.label(name)
                ),
                format!(
                    "{reason}. The workspace runs, but ssh and VS Code cannot connect to it; \
                     restarting the workspace tries again."
                ),
            )
            .about(name.clone()),
        );
    }

    /// Ends `name`'s SSH way in: the entry first, so nothing names a closed endpoint, then the
    /// endpoint, and the problem about it. An entry that cannot be taken out is a problem that
    /// stays: the file names an endpoint that is closed.
    pub(super) async fn close_ssh(&self, name: &WorkspaceId, ssh: SshEndpoint) {
        let key = problem_key("ssh", name);
        match self.inner.ssh.withdraw(name) {
            Ok(()) => self.inner.problems.clear(&key),
            Err(reason) => {
                tracing::warn!(workspace = %name, %reason, "the ssh config still names a closed endpoint");
                self.inner.problems.raise(
                    Problem::new(
                        key,
                        format!(
                            "Puddle's ssh config still names {name}",
                            name = self.label(name)
                        ),
                        format!(
                            "{reason}. The entry points at an endpoint that is now closed, so \
                             `ssh {}` finds nothing; the file is written again, without the \
                             entry, whenever any workspace's direct SSH starts or stops, and this \
                             note ends when this workspace's direct SSH starts or is turned off.",
                            ssh_host(name)
                        ),
                    )
                    .about(name.clone()),
                );
            }
        }
        ssh.close().await;
    }

    /// Makes every running sandbox's SSH endpoint and ssh config entry match its direct SSH
    /// setting, at once and without a restart.
    pub(super) async fn apply_direct_ssh(&self) {
        type Running<S> = (WorkspaceId, Option<Arc<GatedSandbox<S>>>, bool);
        let running: Vec<Running<R::Sandbox>> = {
            let live = self.inner.live.lock().await;
            live.iter()
                .filter_map(|(name, entry)| {
                    Some((
                        name.clone(),
                        Some(entry.gated.clone()?),
                        entry.ssh.is_some(),
                    ))
                })
                .collect()
        };
        for (name, gated, open) in running {
            let allowed = self.direct_ssh_allowed(&name);
            match (allowed, open, gated) {
                (true, false, Some(gated)) => self.open_ssh(&name, gated).await,
                (false, true, _) => {
                    let ssh = self
                        .inner
                        .live
                        .lock()
                        .await
                        .get_mut(&name)
                        .and_then(|entry| entry.ssh.take());
                    if let Some(ssh) = ssh {
                        self.close_ssh(&name, ssh).await;
                    }
                }
                (false, false, _) => self.inner.problems.clear(&problem_key("ssh", &name)),
                _ => {}
            }
        }
    }

    /// Stops what a failed boot or a finished stop leaves: the SSH endpoint and the lifecycle's
    /// handle. The route stays (the sandbox may start again); `drop_route` ends it too.
    pub(super) async fn quiesce(&self, name: &WorkspaceId, drop_route: bool) {
        self.inner.lifecycle.release(&name.sandbox_name());
        let reason = self.terminal_gone(name);
        self.inner.terminals.end(name, reason);
        // Its forwards close; the pinned ones come back at the next start.
        self.inner.ports.stopped(name).await;
        self.inner.flavours.stopped(name);
        // The CA belongs to one start: a stopped or failed sandbox decrypts nothing.
        self.inner.injection.end(name);
        self.inner.problems.clear(&problem_key("ssh", name));
        GitProblem::clear_all(&self.inner.problems, name);
        self.inner.problems.clear(&kubernetes_key(name));
        self.inner.problems.clear_prefix(&merge_notice_prefix(name));
        let (ssh, route) = {
            let mut live = self.inner.live.lock().await;
            if drop_route {
                match live.remove(name) {
                    Some(entry) => (entry.ssh, Some(entry.route)),
                    None => (None, None),
                }
            } else {
                match live.get_mut(name) {
                    Some(entry) => {
                        entry.gated = None;
                        entry.guest = None;
                        (entry.ssh.take(), None)
                    }
                    None => (None, None),
                }
            }
        };
        if let Some(ssh) = ssh {
            self.close_ssh(name, ssh).await;
        }
        if let Some(route) = route {
            route.shutdown().await;
        }
    }

    /// Takes a sandbox that failed after it booted all the way down again, and returns `reason`
    /// with whatever could not be taken down added.
    pub(super) async fn undo_boot(
        &self,
        record: &WorkspaceRecord,
        remove: bool,
        created_volume: bool,
        reason: String,
    ) -> String {
        let inner = &self.inner;
        let mut leftovers = Vec::new();
        let name = &record.id;
        let gated = inner
            .live
            .lock()
            .await
            .get(name)
            .and_then(|l| l.gated.clone());
        if let Some(gated) = gated
            && let Err(e) = gated.stop().await
        {
            tracing::warn!(workspace = %name, error = %e, "stopping a failed workspace failed");
            leftovers.push(format!("the machine {name} may still be running ({e})"));
        }
        self.quiesce(name, remove).await;
        self.inner.reports.end(name);
        if remove {
            match inner.runtime.remove(&name.sandbox_name()).await {
                Ok(()) | Err(ComputeError::NotFound { .. }) => {}
                Err(e) => {
                    tracing::warn!(workspace = %name, error = %e, "removing a failed sandbox failed");
                    leftovers.push(format!("the sandbox {name} was not removed ({e})"));
                }
            }
            inner.workspaces.sandbox_removed(&name.sandbox_name());
        }
        if created_volume
            && let Err(e) = inner.runtime.remove_volume(&record.id.volume_name()).await
        {
            tracing::warn!(workspace = %record.id, error = %e, "removing a new volume failed");
            leftovers.push(format!(
                "the new volume {} was not removed ({e})",
                record.id.volume_name()
            ));
        }
        with_leftovers(reason, &leftovers)
    }
}
