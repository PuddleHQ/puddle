// SPDX-License-Identifier: GPL-3.0-or-later
//! Settling a running workspace: plans, environment, shell init, locks and repairs.

use crate::agent_reports::{clear_no_sudo, entrypoint_problem, no_sudo_problem};
use std::sync::{Arc, PoisonError};

use puddle_api::WorkspaceRecord;
use puddle_boot::GatedSandbox;
use puddle_compute::{ComputeError, Runtime, Sandbox as _};
use puddle_store::{Actor, NewWorkspaceOp, WorkspaceOpKind, WorkspaceOpOutcome};
use puddle_types::{Event, EventSink, GuestEnv, Problem, WorkspaceId, WorkspaceStep};

use puddle_workspace::Layout;

use super::{GuestState, HostWorkspaces, problem_key, with_leftovers};
use crate::boot::GuestInputs;

impl<R: Runtime + Clone> HostWorkspaces<R> {
    /// Plans the guest as `guest` says and runs the plan in it. When the hook put the loader of
    /// the shell init back, the user is told, as a boot would.
    pub(super) async fn run_plan(
        &self,
        name: &WorkspaceId,
        gated: &GatedSandbox<R::Sandbox>,
        guest: &GuestState,
    ) -> Result<(), String> {
        self.run_plan_report(name, gated, guest).await.map(|_| ())
    }

    /// As [`Self::run_plan`], with what the hook said.
    pub(super) async fn run_plan_report(
        &self,
        name: &WorkspaceId,
        gated: &GatedSandbox<R::Sandbox>,
        guest: &GuestState,
    ) -> Result<puddle_boot::BootReport, String> {
        let kit = &self.inner.kit;
        let inputs = GuestInputs {
            ca: &guest.ca,
            authors: &guest.authors,
            ssh: guest.ssh.as_ref(),
            env: &guest.env,
            editor: guest.editor.as_ref(),
            dev_cert: guest.dev_cert.as_ref(),
            root: &guest.root,
            start_folder: guest.start_folder.as_ref(),
            editor_file: guest.editor_file.as_ref(),
            session_user: guest.session_user,
            ssh_agent: guest.ssh_agent,
        };
        let (plan, _) = kit.plan(&guest.image, &inputs)?;
        let report = kit
            .hook()
            .run(gated.ungated(), &plan)
            .await
            .map_err(|failure| failure.to_string())?;
        self.announce_loader_replaced(name, &report);
        self.settle_merge_notices(name, &report);
        Ok(report)
    }

    pub(super) fn announce_loader_replaced(
        &self,
        name: &WorkspaceId,
        report: &puddle_boot::BootReport,
    ) {
        if crate::shell_init::loader_replaced(&report.replaced_files()) {
            self.inner.events.emit(Event::WorkspaceShellInitRestored {
                workspace: name.clone(),
            });
        }
    }

    /// Looks at every running workspace again (after events were missed).
    pub(crate) async fn all_git_changed(&self) {
        for name in self.inner.injection.running_workspaces() {
            self.git_changed(&name).await;
        }
    }

    /// A change to `name`'s variables: the registry follows (see `Injection::environment_changed`),
    /// and the running guest's login-shell file is rewritten, so a terminal opened from now on
    /// sees the new variables with no restart. A process that is already running keeps what it
    /// has, and so does a sandbox's own environment (it is fixed when the sandbox is made).
    pub(crate) async fn environment_changed(&self, name: &WorkspaceId) {
        if let Some(env) = self.inner.injection.environment_changed(name).await {
            self.rewrite_env(name, env).await;
        }
    }

    pub(super) fn note_env_missed(&self, name: &WorkspaceId) {
        self.inner
            .env_missed_while_booting
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(name.clone());
    }

    /// Writes `env` into the running guest's login-shell file by running the boot plan again (the
    /// hook is idempotent: only the files that differ change).
    pub(super) async fn rewrite_env(&self, name: &WorkspaceId, env: GuestEnv) {
        let inner = &self.inner;
        // The miss is recorded under the `live` lock: `start` inserts the entry under the same
        // lock before `settle` reads the record, so a change either finds the entry or is seen by
        // `settle`, never neither.
        let rewrite = {
            let live = inner.live.lock().await;
            let Some(entry) = live.get(name) else {
                // Not in the map yet (the boot hook has not returned, so the boot read the
                // variables before this change and `settle` looks again) or stopped (the next
                // start reads them).
                self.note_env_missed(name);
                return;
            };
            Arc::clone(&entry.rewrite)
        };
        let _one_at_a_time = rewrite.lock().await;
        let running = {
            let live = inner.live.lock().await;
            let running = live
                .get(name)
                .and_then(|e| Some((e.gated.clone()?, e.guest.clone()?)));
            if running.is_none() {
                self.note_env_missed(name);
            }
            running
        };
        let Some((gated, guest)) = running else {
            return;
        };
        if guest.env == env {
            return;
        }
        let replanned = GuestState {
            env: env.clone(),
            ..guest
        };
        match self.run_plan(name, &gated, &replanned).await {
            Ok(()) => {
                if let Some(entry) = inner.live.lock().await.get_mut(name)
                    && let Some(state) = entry.guest.as_mut()
                {
                    state.env = env;
                }
            }
            Err(reason) => {
                tracing::warn!(workspace = %name, %reason, "the variables in the workspace's login shells are not updated");
                inner.events.emit(Event::WorkspaceEnvFailed {
                    workspace: name.clone(),
                    reason: format!(
                        "the change is saved, but new terminals in {name} keep the old variables \
                         until it restarts: {reason}",
                        name = self.label(name)
                    ),
                });
            }
        }
    }

    /// Writes the saved shell init of `id` to the folder its guest sees, for a start. A folder that
    /// held something else is put right and remembered, so `settle` tells the user.
    pub(super) fn publish_shell_init(&self, id: &WorkspaceId) -> Result<(), String> {
        let published = self.inner.kit.shell_init().publish(id)?;
        if published == crate::shell_init::Published::Restored {
            self.inner
                .shell_init_changed_inside
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(id.clone());
        }
        Ok(())
    }

    /// The user saved or removed `name`'s shell init: the folder the running guest sees follows
    /// at once. A workspace that is gone is left alone.
    pub(crate) fn shell_init_changed(&self, name: &WorkspaceId) {
        let id = self
            .state()
            .slots
            .iter()
            .find(|(_, slot)| &slot.record.id == name && !slot.deleting)
            .map(|(id, _)| id.clone());
        let Some(id) = id else { return };
        let key = problem_key("shell-init-copy", name);
        match self.inner.kit.shell_init().refresh(&id) {
            Ok(_) => self.inner.problems.clear(&key),
            Err(reason) => {
                tracing::warn!(workspace = %name, %reason, "the workspace's shell init is not updated");
                self.inner.problems.raise(
                    Problem::new(
                        key,
                        format!(
                            "Puddle could not update {name}'s shell init",
                            name = self.label(name)
                        ),
                        format!(
                            "{reason}. The workspace keeps the shell init it had; saving it again, \
                             or restarting the workspace, tries again."
                        ),
                    )
                    .about(name.clone()),
                );
            }
        }
    }

    /// After a boot: the image's entrypoint that failed at once becomes a problem on the workspace
    /// (the start goes on: the workspace is usable without it, and a refusal would lock the user
    /// out of the way to look at it). The start cleared the earlier one ([`ReportDesk::begin`]);
    /// one that fails later is the guest agent's report, in the same words.
    pub(super) fn settle_entrypoint(&self, name: &WorkspaceId, report: &puddle_boot::BootReport) {
        if let Some(failure) = report.entrypoint_failure() {
            tracing::warn!(workspace = %name, %failure, "the image's entrypoint exited at once");
            self.inner
                .problems
                .raise(entrypoint_problem(name, &self.label(name), failure));
        }
    }

    /// After a boot: an image with no `sudo` for a session user that is not root becomes a problem
    /// on the workspace (the start goes on: everything but `sudo` works); a boot that finds `sudo`,
    /// or a root session user, ends the problem of an earlier boot.
    pub(super) fn settle_no_sudo(&self, name: &WorkspaceId, report: &puddle_boot::BootReport) {
        if report.session_user_has_no_sudo() {
            tracing::warn!(workspace = %name, "the image has no sudo for the session user");
            self.inner
                .problems
                .raise(no_sudo_problem(name, &self.label(name)));
        } else {
            clear_no_sudo(&self.inner.problems, name);
        }
    }

    /// After a boot: tell the user when something inside the workspace had changed its shell init
    /// (the folder, or the file that loads it), and check that the saved script still parses and
    /// prints nothing. A script that fails the check does not stop the start: it is a problem on
    /// the workspace, because a start that refuses over a user's own script would lock them out of
    /// the way to fix it.
    pub(super) async fn settle_shell_init(
        &self,
        name: &WorkspaceId,
        gated: &GatedSandbox<R::Sandbox>,
    ) {
        let inner = &self.inner;
        let folder_changed = inner
            .shell_init_changed_inside
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(name);
        if folder_changed {
            inner.events.emit(Event::WorkspaceShellInitRestored {
                workspace: name.clone(),
            });
        } else {
            self.announce_loader_replaced(name, gated.boot_report());
        }
        let key = problem_key("shell-init", name);
        let saved = inner
            .kit
            .shell_init()
            .saved(name)
            .inspect_err(|reason| {
                tracing::warn!(workspace = %name, %reason, "the shell init was not checked");
            })
            .ok()
            .flatten();
        let Some(script) = saved else {
            inner.problems.clear(&key);
            return;
        };
        match self.run_shell_init_check(name, gated, &script).await {
            Ok(None) => inner.problems.clear(&key),
            Ok(Some(why)) => inner.problems.raise(
                Problem::new(
                    key,
                    format!("{name}'s shell init needs fixing", name = self.label(name)),
                    format!(
                        "{why}. The workspace started, but tools that read a shell's output may \
                         fail until you change it on the workspace's Shell init tab."
                    ),
                )
                .about(name.clone()),
            ),
            Err(reason) => {
                tracing::warn!(workspace = %name, %reason, "the shell init was not checked");
                inner.problems.raise(
                    Problem::new(
                        key,
                        format!(
                            "Puddle could not check {name}'s shell init",
                            name = self.label(name)
                        ),
                        format!("{reason}. It will be checked again at the next start."),
                    )
                    .about(name.clone()),
                );
            }
        }
    }

    /// Runs `script` once in a throwaway shell in the guest. `Ok(None)` when it parses and prints
    /// nothing, else what is wrong with it.
    pub(super) async fn run_shell_init_check(
        &self,
        name: &WorkspaceId,
        gated: &GatedSandbox<R::Sandbox>,
        script: &str,
    ) -> Result<Option<String>, String> {
        let user = self.booted_user(name).await;
        match gated
            .ungated()
            .exec(crate::shell_init::check_request(script, user))
            .await
        {
            Ok(output) => match crate::shell_init::verdict(&output)? {
                crate::shell_init::Verdict::Quiet => Ok(None),
                crate::shell_init::Verdict::Refused(why) => Ok(Some(why)),
            },
            Err(ComputeError::ExecTimeout { .. }) => {
                Ok(Some(crate::shell_init::timed_out_message()))
            }
            Err(e) => Err(e.to_string()),
        }
    }

    /// Takes back what a create that failed with `reason` left, and returns the reason, with
    /// what could not be taken back added.
    pub(super) async fn abort_attachment(
        &self,
        attachment: puddle_workspace::Attachment<'_>,
        reason: String,
    ) -> String {
        match attachment.abort(&self.inner.runtime).await {
            Ok(()) => reason,
            Err(e) => {
                tracing::warn!(error = %e, "cleanup after a failed create failed");
                with_leftovers(reason, &[e.to_string()])
            }
        }
    }

    /// Tells the user about stale git locks that could not be cleared, and withdraws the
    /// problem when they were.
    pub(super) fn report_locks(
        &self,
        name: &WorkspaceId,
        locks: Result<puddle_workspace::LockReport, puddle_workspace::WorkspaceError>,
    ) {
        let key = problem_key("git-locks", name);
        if let Ok(report) = &locks {
            self.report_git_repairs(name, report);
        }
        let why = match locks {
            Ok(report) if report.errors.is_empty() => {
                tracing::debug!(workspace = %name, ?report, "after boot");
                self.inner.problems.clear(&key);
                return;
            }
            Ok(report) => report.errors.join("; "),
            Err(e) => e.to_string(),
        };
        self.inner.problems.raise(
            Problem::new(
                key,
                format!(
                    "Puddle could not clear the stale git lock files in {name}",
                    name = self.label(name)
                ),
                format!(
                    "{why}. The workspace started, but git may say \"Another git process seems to \
                     be running\" until the .lock files in the repository's .git folder are \
                     deleted by hand."
                ),
            )
            .about(name.clone()),
        );
    }

    /// Audits each git file the boot rebuilt, and tells the user what was repaired and what was
    /// found damaged and left alone; a boot that finds neither withdraws the notice.
    pub(super) fn report_git_repairs(
        &self,
        name: &WorkspaceId,
        report: &puddle_workspace::LockReport,
    ) {
        for repair in &report.repaired {
            self.inner.store.note_workspace_op(&NewWorkspaceOp {
                op: WorkspaceOpKind::GitRepaired,
                workspace: Some(name.clone()),
                user: None,
                outcome: WorkspaceOpOutcome::Succeeded,
                actor: Actor::System,
                detail: Some(repair.clone()),
            });
        }
        let key = problem_key("git-repair", name);
        if report.repaired.is_empty() && report.damaged.is_empty() {
            self.inner.problems.clear(&key);
            return;
        }
        let mut parts = Vec::new();
        if !report.repaired.is_empty() {
            parts.push(format!(
                "The last shutdown left git files unreadable. Puddle repaired them: {}. Your files \
                 are unchanged; anything you had staged with git add and not committed is no longer \
                 staged, so run git status and add it again.",
                report.repaired.join("; ")
            ));
        }
        if !report.damaged.is_empty() {
            parts.push(format!(
                "Puddle found damage it could not repair safely and left it as it is: {}. Git will \
                 not work in that repository until it is fixed by hand.",
                report.damaged.join("; ")
            ));
        }
        let detail = parts.join(" ");
        let title = if report.damaged.is_empty() {
            format!(
                "Puddle repaired git files in {name} after the last shutdown",
                name = self.label(name)
            )
        } else {
            format!(
                "Puddle found damaged git files in {name} that it could not repair",
                name = self.label(name)
            )
        };
        self.inner
            .problems
            .raise(Problem::new(key, title, detail).about(name.clone()));
    }

    /// After a boot: Puddle's directory and stale locks on the volume, the notice and check for the
    /// shell init, the SSH endpoint, and the handle the lifecycle stops at exit.
    pub(super) async fn settle(&self, record: &WorkspaceRecord) -> Result<(), String> {
        let inner = &self.inner;
        let (id, name) = (&record.id, &record.id);
        self.progress(name, WorkspaceStep::Syncing, None);
        let gated = {
            let live = inner.live.lock().await;
            live.get(name)
                .and_then(|l| l.gated.clone())
                .ok_or_else(|| format!("{name} is not running"))?
        };
        let locks = inner
            .workspaces
            .after_boot(gated.ungated(), id, self.booted_user(name).await)
            .await
            .map_err(|e| e.to_string())?;
        self.report_locks(name, locks);
        self.settle_shell_init(name, &gated).await;
        self.settle_entrypoint(name, gated.boot_report());
        self.settle_no_sudo(name, gated.boot_report());
        self.settle_kubernetes(name, gated.boot_report());
        self.settle_merge_notices(name, gated.boot_report());
        let authors = inner
            .live
            .lock()
            .await
            .get(name)
            .and_then(|live| live.guest.as_ref().map(|guest| guest.authors.clone()));
        if let Some(authors) = authors {
            self.settle_git_version(name, gated.boot_report(), &authors);
        }
        let root = Layout::new(id).map_err(|e| e.to_string())?.mount().clone();
        self.check_start_folder(name, &gated, &root, record.start_folder.as_ref())
            .await;
        let mount = Layout::new(id).map_err(|e| e.to_string())?.mount().clone();
        let handle = inner
            .runtime
            .get(&name.sandbox_name())
            .await
            .map_err(|e| e.to_string())?;
        if inner.lifecycle.manage(handle, vec![mount]).is_err() {
            return Err("Puddle is shutting down".to_owned());
        }
        // Direct SSH is off unless the user allowed it: then there is no endpoint to connect to
        // and no ssh config entry, however the guest behaves.
        if self.direct_ssh_allowed(name) {
            self.open_ssh(name, gated).await;
        }
        // A change made while the guest booted found no guest to rewrite: look again.
        self.git_changed(name).await;
        let missed = self
            .inner
            .env_missed_while_booting
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(name);
        if missed {
            self.environment_changed(name).await;
        }
        // From here ports can be forwarded, and the pinned forwards come back.
        self.inner.ports.started(name).await;
        Ok(())
    }
}
