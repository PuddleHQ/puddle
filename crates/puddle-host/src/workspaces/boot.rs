// SPDX-License-Identifier: GPL-3.0-or-later
//! Booting a workspace's sandbox and syncing git and editor state into it.

use std::sync::Arc;
use std::time::Duration;

use puddle_api::{StartFolder, WorkspaceRecord};
use puddle_boot::{Gate, GatedSandbox};
use puddle_compute::{DiskSize, ExecRequest, ImageConfig, Runtime, Sandbox as _};
use puddle_types::{
    CheckoutDir, GuestEnv, GuestFile, GuestPath, ImageRef, Problem, SessionUser, WorkspaceId,
    WorkspaceStep,
};

use puddle_workspace::Layout;

use super::{GuestState, HostWorkspaces, Live, boot_message, editor_file_title, problem_key};
use crate::boot::{GuestInputs, root_disk_size};
use crate::git_hosts::{
    Authors, GitProblem, SshListing, authors, credentials_problem, left_out_problem,
    never_decrypted, old_git_problem,
};
use crate::kubernetes::kubernetes_problem;
use crate::merge_notice::settle_merge_notices;

impl<R: Runtime + Clone> HostWorkspaces<R> {
    pub(super) async fn boot_new(
        &self,
        record: &WorkspaceRecord,
        created_volume: &mut bool,
        announce_pull: bool,
        fresh: bool,
    ) -> Result<(), String> {
        let inner = &self.inner;
        let (id, name) = (&record.id, &record.id);
        let size = DiskSize::mib(u32::try_from(record.disk_size_mib).unwrap_or(u32::MAX));
        if announce_pull {
            self.progress(name, WorkspaceStep::PreparingVolume, None);
        }
        // A start must find the volume it left; only a create makes one.
        let attachment = if fresh {
            inner
                .workspaces
                .prepare(&inner.runtime, id, &name.sandbox_name(), Some(size))
                .await
        } else {
            inner
                .workspaces
                .prepare_existing(&inner.runtime, id, &name.sandbox_name())
                .await
        }
        .map_err(|e| {
            if matches!(e, puddle_workspace::WorkspaceError::VolumeMissing { .. }) {
                self.mark_volume_missing(id);
            }
            e.to_string()
        })?;
        *created_volume = attachment.created_volume();
        let image = ImageRef::new(&record.image).map_err(|e| e.to_string());
        let config = match &image {
            Ok(image) => {
                if announce_pull {
                    self.progress(
                        name,
                        WorkspaceStep::PullingImage,
                        Some(record.image.clone()),
                    );
                }
                self.image_config(image).await
            }
            Err(e) => Err(e.clone()),
        };
        let config = match config {
            Ok(config) => config,
            Err(reason) => return Err(self.abort_attachment(attachment, reason).await),
        };
        let image = image.map_err(|e| e.clone())?;
        self.progress(name, WorkspaceStep::Starting, None);
        if let Err(reason) = self.publish_shell_init(id) {
            return Err(self.abort_attachment(attachment, reason).await);
        }
        let route = match inner.kit.route(name) {
            Ok(route) => route,
            Err(reason) => return Err(self.abort_attachment(attachment, reason).await),
        };
        // The CA comes after the route: a start that fails here has nothing else to take back.
        let (plan, env, guest) = match self.plan_guest_for(record, config).await {
            Ok(ready) => ready,
            Err(reason) => {
                route.shutdown().await;
                return Err(self.abort_attachment(attachment, reason).await);
            }
        };
        let spec = attachment
            .add_to(
                inner
                    .kit
                    .spec(name, image, record.memory, &env, &route)
                    .with_root_disk(root_disk_size(record.root_disk_grown_mib))
                    .with_cpus(self.cpus(name).get()),
            )
            .with_file_mount(inner.kit.shell_init().mount(id));
        let gate = Gate::new();
        match inner
            .kit
            .hook()
            .create(&inner.runtime, spec, &plan, &gate)
            .await
        {
            Ok(gated) => {
                attachment.commit();
                inner.flavours.running(name, guest.editor.as_ref());
                inner.live.lock().await.insert(
                    name.clone(),
                    Live {
                        route,
                        gate,
                        gated: Some(Arc::new(gated)),
                        ssh: None,
                        spec_env: guest.env.clone(),
                        guest: Some(guest),
                        rewrite: Arc::default(),
                    },
                );
                Ok(())
            }
            Err(e) => {
                inner.injection.end(name);
                let reason = self.abort_attachment(attachment, boot_message(&e)).await;
                route.shutdown().await;
                Err(reason)
            }
        }
    }

    /// [`Self::plan_guest`] for `record`'s workspace and saved start folder.
    pub(super) async fn plan_guest_for(
        &self,
        record: &WorkspaceRecord,
        image: ImageConfig,
    ) -> Result<(puddle_boot::BootPlan, GuestEnv, GuestState), String> {
        self.plan_guest(
            &record.id,
            &record.id,
            image,
            record.start_folder.as_ref(),
            record.session_user,
        )
        .await
    }

    /// Makes this start's CA for `name`, registers what it decrypts and its secrets' stand-ins and
    /// plans the guest's boot with the CA in its trust, the authors of the workspace's identities
    /// and its environment. A start that fails after this must call `Injection::end`.
    pub(super) async fn plan_guest(
        &self,
        id: &WorkspaceId,
        name: &WorkspaceId,
        image: ImageConfig,
        start_folder: Option<&StartFolder>,
        session_user: SessionUser,
    ) -> Result<(puddle_boot::BootPlan, GuestEnv, GuestState), String> {
        let inner = &self.inner;
        let capture = crate::logins::capture_enabled(inner.settings.as_ref(), name);
        let editor = inner
            .kit
            .editor(id, self.reconnection_grace(name), self.editor_kind())?
            .map(|server| server.with_user(session_user));
        let began = inner.injection.begin(name, capture).await?;
        let wanted = authors(&began.git);
        self.report_git_settings(name, &began.git, &wanted);
        let root = Layout::new(id).map_err(|e| e.to_string())?.mount().clone();
        // Only a running guest can list the checkouts, so the multi-root file the last attach
        // wrote stays as it is and the editor server opens it at this start; the next attach
        // writes it again. A start folder replaces it.
        let editor_file = start_folder
            .is_none()
            .then(|| crate::editor_file::workspace_file_path(&root, id))
            .flatten()
            .map(GuestFile::kept);
        let state = GuestState {
            editor,
            image,
            ca: began.certificate,
            authors: wanted,
            ssh: began.ssh,
            env: began.env,
            dev_cert: inner.kit.dev_cert().await,
            start_folder: start_folder.cloned(),
            root,
            editor_file,
            session_user,
        };
        match inner.kit.plan(
            &state.image,
            &GuestInputs {
                ca: &state.ca,
                authors: &state.authors,
                ssh: state.ssh.as_ref(),
                env: &state.env,
                editor: state.editor.as_ref(),
                dev_cert: state.dev_cert.as_ref(),
                root: &state.root,
                start_folder: state.start_folder.as_ref(),
                editor_file: state.editor_file.as_ref(),
                session_user: state.session_user,
            },
        ) {
            Ok((plan, env)) => Ok((plan, env, state)),
            Err(reason) => {
                inner.injection.end(name);
                Err(reason)
            }
        }
    }

    /// A change to `name`'s Git settings (an identity attached, detached or edited, a coverage
    /// changed): what the running workspace decrypts follows at once, and its guest gets the
    /// authors of the new identities. A workspace that does not run reads the settings at its
    /// next start.
    pub(crate) async fn git_changed(&self, name: &WorkspaceId) {
        if self.decrypt_changed(name).is_some() {
            self.rewrite_authors(name).await;
        }
    }

    /// Recomputes what the running `name` decrypts from the database: the next connection sees
    /// the new set. The settings of a workspace that does not run, or that cannot be read, change
    /// nothing here.
    pub(crate) fn decrypt_changed(&self, name: &WorkspaceId) -> Option<puddle_store::WorkspaceGit> {
        let problems = &self.inner.problems;
        match self.inner.injection.resync(name) {
            Ok(git) => {
                GitProblem::DecryptSync.settle(problems, name, None);
                if let Some(git) = &git {
                    self.report_credentials(name, git);
                }
                git
            }
            Err(reason) => {
                GitProblem::DecryptSync.settle(
                    problems,
                    name,
                    Some((
                        format!(
                            "Puddle could not update what {name} decrypts",
                            name = self.label(name)
                        ),
                        format!(
                            "{reason}. {name} keeps the credentials it had; the next change to its \
                             identities tries again.",
                            name = self.label(name)
                        ),
                    )),
                );
                None
            }
        }
    }

    /// Tells the user about the credentials of `git` that Puddle never uses.
    pub(super) fn report_credentials(&self, name: &WorkspaceId, git: &puddle_store::WorkspaceGit) {
        let hosts = never_decrypted(git);
        GitProblem::Credentials.settle(
            &self.inner.problems,
            name,
            credentials_problem(&self.label(name), &hosts),
        );
    }

    /// Tells the user about what the Git settings of `name` ask for that Puddle cannot carry out.
    pub(super) fn report_git_settings(
        &self,
        name: &WorkspaceId,
        git: &puddle_store::WorkspaceGit,
        wanted: &Authors,
    ) {
        self.report_credentials(name, git);
        GitProblem::LeftOut.settle(
            &self.inner.problems,
            name,
            left_out_problem(&self.label(name), wanted),
        );
    }

    /// After a boot: Kubernetes tools in the guest are a problem on the workspace, since its
    /// kernel cannot route their Services. A stop ends it, so a start that finds none has none.
    pub(super) fn settle_kubernetes(&self, name: &WorkspaceId, report: &puddle_boot::BootReport) {
        if let Some(problem) =
            kubernetes_problem(name, &self.label(name), &report.kubernetes_tools())
        {
            self.inner.problems.raise(problem);
        }
    }

    /// After the hook ran (a boot or a rewrite of the plan in the running guest): each file of the
    /// user's that Puddle merges settings into and could not parse is a notice on the workspace.
    /// The file is left as it is, and the run goes on. A file the hook no longer leaves ends its
    /// notice, so a repair is noticed at the next run and a stop ends them all.
    pub(super) fn settle_merge_notices(
        &self,
        name: &WorkspaceId,
        report: &puddle_boot::BootReport,
    ) {
        settle_merge_notices(
            &self.inner.problems,
            name,
            &self.label(name),
            &report.left_files(),
        );
    }

    /// After the hook ran with `wanted` authors: a guest whose git is too old for the author rules
    /// is a problem on the workspace.
    pub(super) fn settle_git_version(
        &self,
        name: &WorkspaceId,
        report: &puddle_boot::BootReport,
        wanted: &Authors,
    ) {
        GitProblem::OldGit.settle(
            &self.inner.problems,
            name,
            old_git_problem(&self.label(name), report.git_version(), wanted),
        );
    }

    /// Runs the boot plan again in the running guest when its authors differ from what the guest
    /// holds (the hook is idempotent: only the author files change).
    ///
    /// The settings are read once the guest is ours alone: rewrites for one workspace start in
    /// any order, and a copy taken when the change arrived could be applied after a newer one,
    /// leaving the old authors and a problem the newer settings already ended.
    pub(crate) async fn rewrite_authors(&self, name: &WorkspaceId) {
        let inner = &self.inner;
        let _held = match &inner.rewrite_gate {
            Some(gate) => Some(gate(name).await),
            None => None,
        };
        let rewrite = {
            let live = inner.live.lock().await;
            match live.get(name) {
                Some(entry) => Arc::clone(&entry.rewrite),
                None => return, // not registered yet
            }
        };
        let _one_at_a_time = rewrite.lock().await;
        let (gated, guest) = {
            let live = inner.live.lock().await;
            match live
                .get(name)
                .and_then(|e| Some((e.gated.clone()?, e.guest.clone()?)))
            {
                Some(running) => running,
                // Not booted yet (the start reads the settings itself, and `settle` looks again)
                // or stopped.
                None => return, // not up yet
            }
        };
        // The caller's own read of the settings worked, so this one failing is news: say so.
        let git = match inner.injection.git(name) {
            Ok(git) => git,
            Err(reason) => {
                self.raise_author_sync(name, &reason);
                return;
            }
        };
        let wanted = authors(&git);
        self.report_git_settings(name, &git, &wanted);
        // The keys stay the start's; the names they are listed under follow the identities.
        let listing = guest
            .ssh
            .as_ref()
            .map(|held| SshListing::for_git(Arc::clone(&held.keys), &git));
        if guest.authors == wanted
            && guest.ssh.as_ref().map(|s| &s.hosts) == listing.as_ref().map(|s| &s.hosts)
        {
            return;
        }
        let replanned = GuestState {
            authors: wanted.clone(),
            ssh: listing.clone(),
            ..guest
        };
        match self.run_plan_report(name, &gated, &replanned).await {
            Ok(report) => {
                GitProblem::AuthorSync.settle(&inner.problems, name, None);
                self.settle_git_version(name, &report, &wanted);
                if let Some(entry) = inner.live.lock().await.get_mut(name)
                    && let Some(state) = entry.guest.as_mut()
                {
                    state.authors = wanted;
                    state.ssh = listing;
                }
            }
            Err(reason) => self.raise_author_sync(name, &reason),
        }
    }

    /// Tells the user the commit authors in `name` could not be updated and why.
    pub(super) fn raise_author_sync(&self, name: &WorkspaceId, reason: &str) {
        GitProblem::AuthorSync.settle(
            &self.inner.problems,
            name,
            Some((
                format!(
                    "Puddle could not update the commit authors in {name}",
                    name = self.label(name)
                ),
                format!(
                    "{reason}. The workspace keeps the authors it had; the next change to its \
                     identities tries again."
                ),
            )),
        );
    }

    /// Looks in the guest for the start folder. A missing one never fails anything:
    /// terminals and the editor open the volume root, and a problem on the workspace names the
    /// folder and the ways out until it exists or the setting changes. `None` for the volume
    /// root, or when the guest could not be asked (the problem stays as it was).
    pub(super) async fn check_start_folder(
        &self,
        name: &WorkspaceId,
        gated: &GatedSandbox<R::Sandbox>,
        root: &GuestPath,
        start: Option<&StartFolder>,
    ) -> Option<bool> {
        let key = problem_key("start-folder", name);
        let Some(folder) = start else {
            self.inner.problems.clear(&key);
            return None;
        };
        let path = crate::editor_file::start_path(root, start);
        let request = ExecRequest::new(
            "sh",
            ["-c", "[ -d \"$1\" ]", "puddle-start-folder", path.as_str()].map(str::to_owned),
        )
        .as_user("root")
        .with_timeout(Duration::from_secs(10));
        let exists = match gated.ungated().exec(request).await {
            Ok(output) => output.status.success(),
            Err(e) => {
                tracing::warn!(workspace = %name, error = %e, "the start folder was not looked for");
                self.inner.problems.raise(
                    Problem::new(
                        key,
                        format!(
                            "Puddle could not look for {name}'s start folder {folder}",
                            name = self.label(name)
                        ),
                        format!(
                            "{e}. New terminals and the editor may not open it; Puddle looks \
                             again at every start and attach."
                        ),
                    )
                    .about(name.clone()),
                );
                return None;
            }
        };
        if exists {
            self.inner.problems.clear(&key);
        } else {
            self.inner.problems.raise(
                Problem::new(
                    key,
                    format!(
                        "{name}'s start folder {folder} does not exist",
                        name = self.label(name)
                    ),
                    format!(
                        "New terminals and the editor open the volume root instead. Create the \
                         folder {folder} in the workspace, or change the start folder in the \
                         workspace's settings; Puddle looks again at every start and attach."
                    ),
                )
                .about(name.clone()),
            );
        }
        Some(exists)
    }

    /// The start folder of the running `record` changed: the login-shell file is written again
    /// (the hook is idempotent) and the folder is looked for. `None` when the workspace does not
    /// run, or the folder cannot be looked for.
    pub(super) async fn follow_start_folder(&self, record: &WorkspaceRecord) -> Option<bool> {
        let name = &record.id;
        let inner = &self.inner;
        let rewrite = inner
            .live
            .lock()
            .await
            .get(name)
            .map(|e| Arc::clone(&e.rewrite))?;
        let _one_at_a_time = rewrite.lock().await;
        let (gated, guest) = {
            let live = inner.live.lock().await;
            live.get(name)
                .and_then(|e| Some((e.gated.clone()?, e.guest.clone()?)))?
        };
        let replanned = GuestState {
            start_folder: record.start_folder.clone(),
            // Opening a folder replaces the multi-root window; the next attach writes it again.
            editor_file: record
                .start_folder
                .as_ref()
                .and(None)
                .or(guest.editor_file.clone()),
            ..guest
        };
        let key = problem_key("start-folder-file", name);
        match self.run_plan(name, &gated, &replanned).await {
            Ok(()) => {
                inner.problems.clear(&key);
                if let Some(state) = inner
                    .live
                    .lock()
                    .await
                    .get_mut(name)
                    .and_then(|e| e.guest.as_mut())
                {
                    state.start_folder.clone_from(&replanned.start_folder);
                    state.editor_file.clone_from(&replanned.editor_file);
                }
            }
            Err(reason) => {
                tracing::warn!(workspace = %name, %reason, "the start folder is not updated in the guest");
                inner.problems.raise(
                    Problem::new(
                        key,
                        format!(
                            "Puddle could not move {name}'s terminals to the new start folder",
                            name = self.label(name)
                        ),
                        format!(
                            "{reason}. The setting is saved; new terminals start where they did \
                             until the workspace starts again."
                        ),
                    )
                    .about(name.clone()),
                );
            }
        }
        self.check_start_folder(name, &gated, &replanned.root, record.start_folder.as_ref())
            .await
    }

    /// At an attach: the editor's target. With no start folder and two or more checkouts the
    /// multi-root file is written again (through the boot hook, which merges only `folders` and
    /// leaves a file it cannot parse as it is, with a problem naming it); with fewer, Puddle's
    /// lines are taken out of it.
    pub(super) async fn refresh_editor_file(&self, record: &WorkspaceRecord) {
        let name = &record.id;
        let inner = &self.inner;
        let Some(rewrite) = inner
            .live
            .lock()
            .await
            .get(name)
            .map(|e| Arc::clone(&e.rewrite))
        else {
            return;
        };
        let _one_at_a_time = rewrite.lock().await;
        let Some((gated, guest)) = ({
            let live = inner.live.lock().await;
            live.get(name)
                .and_then(|e| Some((e.gated.clone()?, e.guest.clone()?)))
        }) else {
            return;
        };
        self.check_start_folder(name, &gated, &guest.root, record.start_folder.as_ref())
            .await;
        let key = problem_key("editor-file", name);
        let not_updated = |reason: &str| {
            tracing::warn!(workspace = %name, %reason, "the editor's target is not updated");
            inner.problems.raise(
                Problem::new(
                    key.clone(),
                    editor_file_title(record),
                    format!(
                        "{reason}. The editor opens what it opened before until the next attach \
                         works."
                    ),
                )
                .about(name.clone()),
            );
        };
        let found = match inner
            .workspaces
            .list_checkouts(gated.ungated(), &record.id)
            .await
        {
            Ok(list) => crate::editor_file::Found::from_listing(&list),
            Err(e) => {
                not_updated(&e.to_string());
                return;
            }
        };
        let order: Vec<CheckoutDir> = record
            .repositories
            .iter()
            .map(|r| r.directory.clone())
            .collect();
        let file = crate::editor_file::multi_root_folders(
            &guest.root,
            record.start_folder.as_ref(),
            &found,
            &order,
        )
        .and_then(|folders| crate::editor_file::workspace_file(&guest.root, &record.id, &folders));
        let replanned = GuestState {
            editor_file: file,
            ..guest.clone()
        };
        let report = match self.run_plan_report(name, &gated, &replanned).await {
            Ok(report) => report,
            Err(reason) => {
                not_updated(&reason);
                return;
            }
        };
        if let Some(state) = inner
            .live
            .lock()
            .await
            .get_mut(name)
            .and_then(|e| e.guest.as_mut())
        {
            state.editor_file.clone_from(&replanned.editor_file);
        }
        let left = report.left_files();
        match left
            .iter()
            .find(|(path, _)| path.ends_with(".code-workspace"))
        {
            Some((path, why)) => inner.problems.raise(
                Problem::new(
                    key,
                    editor_file_title(record),
                    format!(
                        "{path} was left as it is: {why}. The editor opens it as it is, or \
                         the volume root when it lists no folder, until the file is fixed or \
                         deleted."
                    ),
                )
                .about(name.clone()),
            ),
            _ => inner.problems.clear(&key),
        }
    }
}
