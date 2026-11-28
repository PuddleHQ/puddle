// SPDX-License-Identifier: AGPL-3.0-or-later
use super::{
    CheckoutDir, ComputeError, CpuCount, DiskSize, ExecRequest, FileMount, ImageRef, Import,
    Layout, Loan, Runtime, Sandbox, SandboxSpec, Scratch, SessionUser, VolumeInfo, VolumeMount,
    WorkspaceError, WorkspaceId, Workspaces, clear_leftover, debug, import_mount, info,
    maintenance_name, mount_for, tail, warn, with_scratch_sandbox,
};

/// What [`Workspaces::import`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportReport {
    /// Whether the import made the workspace's volume (a later failure of the create removes it).
    pub created_volume: bool,
    /// The checkouts git refused, each with git's reason; the others are on the volume.
    pub failed: Vec<(CheckoutDir, String)>,
}

/// Who an import runs as and what it clones.
#[derive(Clone, Copy)]
struct ImportRun<'a> {
    user: SessionUser,
    imports: &'a [Import],
}

impl Workspaces {
    /// Clones the checkouts `imports` (on this computer) onto workspace `id`'s volume, making
    /// the volume with `size` if it does not exist yet. It runs in the workspace's short-lived
    /// maintenance sandbox, booted from `image` (the workspace's own, so the clones belong to the
    /// user its sandbox runs commands as): no route, the volume, and each checkout's `.git`
    /// read-only at [`import_mount`]. Each clone gets every branch and tag of the checkout and
    /// `origin` set to its [`Import::url`] (see [`crate::FINISH_IMPORT_SH`]); one `sync` follows.
    /// The sandbox is removed again whatever happened. A checkout git refuses fails only itself:
    /// its folder is removed and the reason is in [`ImportReport::failed`].
    ///
    /// # Errors
    ///
    /// [`WorkspaceError::InUse`]; [`WorkspaceError::Runtime`]; [`WorkspaceError::Sync`];
    /// [`WorkspaceError::Leftover`]. A volume this call made is removed again on an error.
    pub async fn import<R: Runtime>(
        &self,
        rt: &R,
        id: &WorkspaceId,
        image: &ImageRef,
        size: DiskSize,
        user: SessionUser,
        imports: &[Import],
    ) -> Result<ImportReport, WorkspaceError> {
        let name = maintenance_name(id)?;
        self.borrow(id, &name)?;
        // Held to the end, cleanup included, and given back when the request is dropped too.
        let _loan = Loan {
            registry: &self.registry,
            id,
        };
        let volume = id.volume_name();
        let existing = rt
            .volume(&volume)
            .await
            .map_err(|e| WorkspaceError::runtime("look up the volume", id, e))?;
        let existed = existing.is_some();
        let result = self
            .run_import(
                rt,
                id,
                image,
                existing.as_ref(),
                size,
                ImportRun { user, imports },
            )
            .await;
        // The create makes the volume; whether it got that far is read, not assumed.
        let made = if existed {
            Ok(false)
        } else {
            rt.volume(&volume).await.map(|found| found.is_some())
        };
        match result {
            // A lookup that fails after a good import: the import ran on the volume it made.
            Ok(failed) => Ok(ImportReport {
                created_volume: made.unwrap_or(true),
                failed,
            }),
            // The sandbox is still there and holds the volume: removing it now would fail or
            // pull it from under a running guest. The error names the sandbox left behind.
            Err(e @ WorkspaceError::Leftover { .. }) => Err(e),
            Err(e) => match made {
                Ok(true) => match rt.remove_volume(&volume).await {
                    Ok(()) => Err(e),
                    Err(left) => Err(WorkspaceError::Leftover {
                        workspace: id.to_string(),
                        job: e.to_string(),
                        leftover: format!("the new volume {volume} was not removed ({left})"),
                    }),
                },
                Ok(false) => Err(e),
                Err(lookup) => Err(WorkspaceError::Leftover {
                    workspace: id.to_string(),
                    job: e.to_string(),
                    leftover: format!(
                        "the new volume {volume} may not have been removed (looking it up failed: {lookup})"
                    ),
                }),
            },
        }
    }

    async fn run_import<R: Runtime>(
        &self,
        rt: &R,
        id: &WorkspaceId,
        image: &ImageRef,
        existing: Option<&VolumeInfo>,
        size: DiskSize,
        run: ImportRun<'_>,
    ) -> Result<Vec<(CheckoutDir, String)>, WorkspaceError> {
        let ImportRun { user, imports } = run;
        let name = &maintenance_name(id)?;
        clear_leftover(rt, id, name).await?;
        let layout = Layout::new()?;
        // The size is for a volume the import makes; one that exists keeps its own.
        let mount = match existing {
            Some(info) => {
                if info.size != size {
                    info!(workspace = %id, size = %info.size, "workspace volume exists; keeping its size");
                }
                mount_for(&id.volume_name(), info)?
            }
            None => VolumeMount::named(id.volume_name(), layout.mount().clone()).ensure_size(size),
        };
        let mut spec = SandboxSpec::new(name.clone(), image.clone())
            .with_memory(self.config.maintenance_memory)
            .with_cpus(CpuCount::MIN)
            .with_volume(mount);
        for (n, import) in imports.iter().enumerate() {
            spec = spec.with_file_mount(FileMount::checkout(&import.git_dir, import_mount(n)?));
        }
        debug!(workspace = %id, sandbox = %name, checkouts = imports.len(), "importing checkouts");
        with_scratch_sandbox(rt, id, spec, Scratch::Import, async |sandbox| {
            self.import_into(sandbox, id, &layout, user, imports).await
        })
        .await
    }

    async fn import_into<S: Sandbox>(
        &self,
        sandbox: &S,
        id: &WorkspaceId,
        layout: &Layout,
        user: SessionUser,
        imports: &[Import],
    ) -> Result<Vec<(CheckoutDir, String)>, WorkspaceError> {
        let mut failed = Vec::new();
        for (n, import) in imports.iter().enumerate() {
            let dir = &import.directory;
            // A refusal of git and a step that ran out of time fail only this checkout; anything
            // else (the guest gone) fails the import. The flag says whether git was stopped
            // part way (a timeout), so it removed nothing itself.
            let step = |request: ExecRequest| async move {
                match sandbox.exec(request).await {
                    Ok(out) if out.status.success() => Ok(None),
                    Ok(out) => Ok(Some((
                        format!(
                            "git exited {}: {}",
                            out.status.code,
                            tail(&out.stderr_text())
                        ),
                        false,
                    ))),
                    Err(ComputeError::ExecTimeout { timeout, .. }) => Ok(Some((
                        format!("git did not finish within {} s", timeout.as_secs()),
                        true,
                    ))),
                    Err(e) => Err(WorkspaceError::runtime("import a checkout", id, e)),
                }
            };
            // git removes the folder of a clone it refused; a clone it was stopped in, or whose
            // finishing step failed, is half made.
            let reason = match step(layout.import_request(n, dir)?.as_user(user.name())).await? {
                Some((reason, stopped)) => Some((reason, stopped)),
                None => step(
                    layout
                        .finish_import_request(n, dir, &import.url)?
                        .as_user(user.name()),
                )
                .await?
                .map(|(reason, _)| (reason, true)),
            };
            let Some((mut reason, half_made)) = reason else {
                info!(workspace = %id, checkout = %dir, "checkout imported");
                continue;
            };
            warn!(workspace = %id, checkout = %dir, %reason, "a checkout was not imported");
            // A half-made clone is removed, so a later clone of the same folder is not refused.
            if half_made {
                let checkout = layout.checkout(dir)?;
                let rm = ExecRequest::new("rm", ["-rf", "--", checkout.as_str()]).as_user("root");
                let left = match sandbox.exec(rm).await {
                    Ok(out) if out.status.success() => None,
                    Ok(out) => Some(tail(&out.stderr_text())),
                    Err(e) => Some(e.to_string()),
                };
                if let Some(left) = left {
                    reason = format!("{reason}; its half-made folder was not removed: {left}");
                }
            }
            failed.push((dir.clone(), reason));
        }
        if failed.len() < imports.len() {
            self.sync(sandbox, id).await?;
        }
        Ok(failed)
    }
}
