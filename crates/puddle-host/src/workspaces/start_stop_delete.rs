// SPDX-License-Identifier: GPL-3.0-or-later
//! Starting, stopping and deleting a workspace.

use std::sync::Arc;

use puddle_api::EndReason;
use puddle_api::{DisplayName, WorkspaceError, WorkspaceRecord};
use puddle_boot::{BootError, GatedSandbox};
use puddle_compute::{ComputeError, Runtime, Sandbox as _, StartRefusal};
use puddle_types::{ImageRef, Problem, WorkspaceId, WorkspaceStep};

use super::{HostWorkspaces, RuntimeNames, State, problem_key};
use crate::op_error::OpError;

impl<R: Runtime + Clone> HostWorkspaces<R> {
    pub(super) async fn start_work(&self, record: WorkspaceRecord) -> Result<(), OpError> {
        let inner = &self.inner;
        let name = &record.id;
        self.progress(name, WorkspaceStep::Starting, None);
        inner.problems.clear(&problem_key("forced-stop", name));
        inner.problems.clear(&problem_key("start-refused", name));
        inner.reports.begin(name, &record.name);
        let exists = inner
            .runtime
            .list()
            .await?
            .iter()
            .any(|s| s.name == name.as_str());
        let route_alive = inner.live.lock().await.contains_key(name);
        let restarted = exists && route_alive && self.restart_in_place(&record).await?;
        if exists && route_alive && !restarted {
            // The root disk goes with the old sandbox (home directory, installed packages); the
            // volume with the work stays. Say so: this is Puddle's own doing.
            self.progress(
                name,
                WorkspaceStep::Starting,
                Some(
                    "the workspace's variables could not be applied to its sandbox, so it is made \
                     again; the volume with your work is kept, what was installed outside it is not"
                        .to_owned(),
                ),
            );
        }
        if !restarted {
            if exists {
                inner.runtime.remove(&name.sandbox_name()).await?;
                inner.workspaces.sandbox_removed(&name.sandbox_name());
            }
            // A route left from an earlier boot belongs to a sandbox that no longer exists.
            self.quiesce(name, true).await;
            let mut created_volume = false;
            self.boot_new(&record, &mut created_volume, false, false)
                .await?;
        }
        if let Err(reason) = self.settle(&record).await {
            return Err(self.undo_boot(&record, false, false, reason).await);
        }
        self.refresh_checkouts_later(&record.id);
        Ok(())
    }

    /// Starts the stopped sandbox again, with the workspace's variables as they are now: a changed
    /// set is written to the stopped sandbox first (`Runtime::set_env`), so the root disk (home
    /// directory, installed packages) is kept. `Ok(false)` only when that write failed: the caller
    /// then makes a new sandbox on the same volume, the volume holding the work, not the sandbox.
    pub(super) async fn restart_in_place(&self, record: &WorkspaceRecord) -> Result<bool, OpError> {
        let inner = &self.inner;
        let name = &record.id;
        let image = ImageRef::new(&record.image)?;
        let config = self.image_config(&image).await?;
        self.publish_shell_init(&record.id)?;
        let (plan, spec_env, guest) = self.plan_guest_for(record, config).await?;
        let made_with = inner
            .live
            .lock()
            .await
            .get(name)
            .map(|live| live.spec_env.clone());
        if made_with.is_some_and(|env| env != guest.env) {
            if let Err(e) = inner.runtime.set_env(&name.sandbox_name(), &spec_env).await {
                tracing::warn!(workspace = %name, error = %e, "the changed variables could not be applied to the stopped sandbox");
                inner.injection.end(name);
                return Ok(false);
            }
            if let Some(live) = inner.live.lock().await.get_mut(name) {
                live.spec_env = guest.env.clone();
            }
        }
        let started = self.start_planned(record, &plan).await;
        match started {
            Ok(gated) => {
                inner.flavours.running(name, guest.editor.as_ref());
                if let Some(live) = inner.live.lock().await.get_mut(name) {
                    live.gated = Some(Arc::new(gated));
                    live.guest = Some(guest);
                }
                Ok(true)
            }
            Err(reason) => {
                inner.injection.end(name);
                Err(reason)
            }
        }
    }

    /// Starts the stopped sandbox of `record` through the boot hook with `plan`.
    pub(super) async fn start_planned(
        &self,
        record: &WorkspaceRecord,
        plan: &puddle_boot::BootPlan,
    ) -> Result<GatedSandbox<R::Sandbox>, OpError> {
        let inner = &self.inner;
        let name = &record.id;
        let gate = inner
            .live
            .lock()
            .await
            .get(name)
            .map(|l| l.gate.clone())
            .ok_or_else(|| OpError::NoRoute(name.clone()))?;
        // The memory setting applies at the next start (ADR 0006 and the memory setting).
        inner
            .runtime
            .set_memory(&name.sandbox_name(), record.memory)
            .await?;
        // The cpus setting applies at the next start, like the memory.
        inner
            .runtime
            .set_cpus(&name.sandbox_name(), self.cpus(name))
            .await?;
        inner
            .kit
            .hook()
            .start(&inner.runtime, &name.sandbox_name(), plan, &gate)
            .await
            .map_err(|e| {
                if let BootError::Compute(ComputeError::StartRefused { cause, .. }) = &e {
                    self.raise_start_refused(name, *cause);
                }
                OpError::from(e)
            })
    }

    /// Raises the problem that the runtime refused to start `name` and why.
    pub(super) fn raise_start_refused(&self, name: &WorkspaceId, cause: StartRefusal) {
        self.inner.problems.raise(
            Problem::new(
                problem_key("start-refused", name),
                format!("{name} was not started", name = self.label(name)),
                format!(
                    "The runtime refused to start it, and nothing was attached: {cause}. {}",
                    cause.way_out()
                ),
            )
            .about(name.clone()),
        );
    }

    pub(super) async fn stop_work(&self, record: WorkspaceRecord) -> Result<(), OpError> {
        let name = &record.id;
        // The terminals end first, with the stop as their reason, while the guest can still answer.
        self.inner.terminals.end(name, EndReason::Stopped);
        // The windows go first, so the editor can disconnect and the guest end its extension hosts
        // before the stop (best effort: the close is asked for, not waited for).
        self.inner.launcher.close_browser(name).await;
        let gated = self.gated(name).await?;
        self.progress(name, WorkspaceStep::Reclaiming, None);
        // A failed trim only costs disk space; the VM still stops, and the user is told.
        let trim_key = problem_key("trim", name);
        match self
            .inner
            .workspaces
            .trim(gated.ungated(), &record.id)
            .await
        {
            Ok(_) => self.inner.problems.clear(&trim_key),
            Err(e) => {
                tracing::warn!(workspace = %record.id, error = %e, "trim before stop failed");
                self.inner.problems.raise(
                    Problem::new(
                        trim_key,
                        format!(
                            "{name}'s disk was not trimmed when it stopped",
                            name = self.label(name)
                        ),
                        format!(
                            "{e}. The workspace stopped normally. Its disk file stays larger \
                             than the data in it until a later stop, or Reclaim space, trims it."
                        ),
                    )
                    .about(name.clone()),
                );
            }
        }
        self.progress(name, WorkspaceStep::Stopping, None);
        gated.stop().await?;
        if gated.ungated().stopped_by_force() {
            self.inner.problems.raise(
                Problem::new(
                    problem_key("forced-stop", name),
                    format!(
                        "{name} did not shut down in time and was ended by force",
                        name = self.label(name)
                    ),
                    "Files it had not written to its disk yet may be missing. Start it again \
                     and check the repositories with git status.",
                )
                .about(name.clone()),
            );
        }
        self.quiesce(name, false).await;
        self.inner.reports.end(name);
        Ok(())
    }

    /// Deletes the workspace's settings document. A failure is a notice (the file stays). A new
    /// workspace has a new id, so it never inherits a stuck file.
    /// Under the settings routes' lock, so a settings change that read the file before this
    /// removal cannot write it back after.
    pub(super) async fn forget_settings_file(&self, id: &WorkspaceId, name: &str) {
        let inner = &self.inner;
        let key = problem_key("settings", id);
        let _lock = inner.settings_lock.lock().await;
        match inner.settings.delete_workspace(id) {
            Ok(()) => inner.problems.clear(&key),
            Err(e) => inner.problems.raise(Problem::new(
                key,
                format!("{name}'s settings were not deleted"),
                format!(
                    "{e}. No later workspace gets the id {id}, so none inherits them; delete \
                     the file by hand to tidy up."
                ),
            )),
        }
    }

    /// Deletes what the browser editor kept under `name`: the front's half of Microsoft's secret
    /// key and the windows' browser data. Returns what could not be deleted, one reason each.
    pub(super) async fn clear_browser_state(&self, name: &WorkspaceId) -> Vec<String> {
        let inner = &self.inner;
        let mut failed = Vec::new();
        // The windows first: one still open could mint the key again after it was forgotten.
        if let Err(e) = inner.launcher.forget_browser(name).await {
            failed.push(e.to_string());
        }
        if let Err(e) = inner.front_halves.forget(name) {
            failed.push(format!("the editor's secret key could not be deleted: {e}"));
        }
        failed
    }

    /// The names the runtime has for volumes and sandboxes, which a new id must not meet.
    pub(super) async fn runtime_names(&self) -> Result<RuntimeNames, WorkspaceError> {
        let unavailable = |e: ComputeError| {
            WorkspaceError::Unavailable(format!(
                "Puddle cannot list the runtime's disks and sandboxes, so it cannot pick an id \
                 no earlier workspace used: {e}"
            ))
        };
        let volumes = self
            .inner
            .runtime
            .list_volumes()
            .await
            .map_err(unavailable)?;
        let sandboxes = self.inner.runtime.list().await.map_err(unavailable)?;
        Ok(RuntimeNames {
            volumes: volumes.into_iter().map(|v| v.name).collect(),
            sandboxes: sandboxes.into_iter().map(|s| s.name).collect(),
        })
    }

    /// Whether `id` is in use or was ever used: listed or retired, a volume `ws-<id>`, a
    /// sandbox `<id>` or `m--<id>`, or anything Puddle's data folder keys by it (browser data,
    /// the front's key half, a settings file, a login marker). A settings file that cannot be
    /// read counts as there.
    pub(super) fn id_taken(&self, state: &State, runtime: &RuntimeNames, id: &WorkspaceId) -> bool {
        let inner = &self.inner;
        state.slots.contains_key(id)
            || state.retired.contains(id)
            || runtime.volumes.contains(id.volume_name().as_str())
            || runtime.sandboxes.contains(id.as_str())
            || runtime
                .sandboxes
                .contains(&format!("{}{id}", puddle_workspace::MAINTENANCE_PREFIX))
            || inner.launcher.has_browser_data(id)
            || inner.front_halves.has(id)
            || !matches!(inner.settings.load_workspace(id), Ok(None))
            || puddle_logins::Marker::for_workspace(&inner.login_markers, id).exists()
    }

    /// The editor's origin is `<id>.localhost`, so a workspace with this id would reach the
    /// browser data an earlier one left (a delete cut short, or made where no window could clean
    /// it). No window can exist for a workspace that is still being created: clear it once the id is reserved.
    pub(super) async fn clear_left_over_browser_state(
        &self,
        id: &WorkspaceId,
        name: &DisplayName,
    ) -> Result<(), WorkspaceError> {
        let failed = self.clear_browser_state(id).await;
        if failed.is_empty() {
            self.inner.problems.clear(&problem_key("browser-data", id));
            return Ok(());
        }
        // The create is refused; the id it reserved is retired like a failed create's.
        let mut state = self.state();
        state.slots.remove(id);
        state.retired.insert(id.clone());
        // Saved now: no entry carries this id, so a crash before the next save would forget it.
        if let Err(e) = self.persist(&state) {
            tracing::warn!(workspace = %id, error = %e, "the retired id was not saved after a refused create");
        }
        drop(state);
        Err(WorkspaceError::Conflict(format!(
            "browser data left under the id {id} could not be removed ({}), and the new \
             workspace would inherit it; {name} was not created. Close its editor windows and \
             try again",
            failed.join("; ")
        )))
    }

    /// Deletes what the browser editor kept for a deleted workspace. A failure is a notice: the
    /// data stays under an id no later workspace gets.
    pub(super) async fn forget_browser_state(&self, id: &WorkspaceId, name: &str) {
        let inner = &self.inner;
        let key = problem_key("browser-data", id);
        let failed = self.clear_browser_state(id).await;
        if failed.is_empty() {
            inner.problems.clear(&key);
        } else {
            inner.problems.raise(Problem::new(
                key,
                format!("{name}'s browser editor data was not deleted"),
                format!(
                    "{}. No later workspace gets the id {id}, so none reaches it; to tidy up, \
                     close the editor windows and delete its folders under webview and front in \
                     Puddle's data folder.",
                    failed.join("; ")
                ),
            ));
        }
    }

    pub(super) async fn delete_work(
        &self,
        record: WorkspaceRecord,
        report: puddle_workspace::DeleteReport,
    ) -> Result<(), OpError> {
        let inner = &self.inner;
        self.progress(&record.id, WorkspaceStep::Checking, None);
        inner.launcher.close_browser(&record.id).await;
        self.progress(&record.id, WorkspaceStep::Removing, None);
        inner
            .workspaces
            .delete(&inner.runtime, &record.id, &report.confirm())
            .await?;
        self.quiesce(&record.id, true).await;
        self.inner.reports.forget(&record.id);
        inner.kit.shell_init().forget(&record.id);
        inner
            .problems
            .clear_prefix(&format!("{}:", problem_key("clone", &record.id)));
        self.forget_leftovers(&record.id, record.name.as_str())
            .await;
        Ok(())
    }

    /// Removes what a deleted workspace left under its id outside the runtime: its rules, its
    /// identities, its environment and the values of its own secrets, its settings file and the
    /// browser editor's data. Every step finds nothing the second time, so the sweep at start
    /// can run it again for any retired id; a step that fails is a notice and the next start
    /// tries again.
    pub(super) async fn forget_leftovers(&self, id: &WorkspaceId, label: &str) {
        self.inner.injection.forget(id).await;
        self.forget_settings_file(id, label).await;
        self.forget_browser_state(id, label).await;
    }

    /// Finishes what a delete that did not end left behind, for every retired id: a sandbox
    /// still named after it, and the leftovers [`Self::forget_leftovers`] removes. Run once in
    /// the background at start. A retired id belongs to nobody, so nothing here can touch a
    /// workspace the user has.
    pub(crate) async fn sweep_retired(&self) {
        let retired: Vec<WorkspaceId> = self.state().retired.iter().cloned().collect();
        for id in retired {
            self.remove_left_sandbox(&id).await;
            self.forget_leftovers(&id, id.as_str()).await;
        }
    }

    /// Removes the sandbox a deleted workspace left, if there is one. A failure is a notice.
    pub(super) async fn remove_left_sandbox(&self, id: &WorkspaceId) {
        let key = problem_key("left-sandbox", id);
        match self.inner.runtime.remove(&id.sandbox_name()).await {
            Ok(()) | Err(ComputeError::NotFound { .. }) => self.inner.problems.clear(&key),
            Err(e) => self.inner.problems.raise(Problem::new(
                key,
                format!("The sandbox of the deleted workspace {id} was not removed"),
                format!("{e}. Puddle tries again the next time it starts."),
            )),
        }
    }
}
