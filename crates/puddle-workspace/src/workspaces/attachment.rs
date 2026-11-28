// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

/// A workspace reserved for a sandbox that is being created (from [`Workspaces::prepare`]).
/// Finish it with [`Attachment::commit`] or [`Attachment::abort`]; dropping it unfinished only
/// releases the reservation (and logs a warning), leaving the runtime as it is.
#[derive(Debug)]
#[must_use = "commit or abort the attachment"]
pub struct Attachment<'w> {
    reservation: Reservation<'w>,
    mount: VolumeMount,
    created_volume: bool,
    kept_size: Option<KeptSize>,
    before: Before,
}

/// The claim a sandbox has on a workspace until it commits or aborts; dropping it while open
/// releases the claim.
#[derive(Debug)]
struct Reservation<'w> {
    workspaces: &'w Workspaces,
    id: WorkspaceId,
    sandbox: SandboxName,
    open: bool,
    /// The sandbox held the workspace attached before this reservation: dropping it open
    /// restores that, so a failed recreate does not leave the workspace unowned.
    was_attached: bool,
}

impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        if self.open {
            if self.was_attached {
                self.workspaces.registry.attach(&self.id, &self.sandbox);
            } else {
                self.workspaces.registry.release(&self.id, &self.sandbox);
            }
        }
    }
}

/// A volume that already existed with another size than the one asked for. A volume only ever
/// grows through its own resize, so the existing size stays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeptSize {
    /// The size the caller asked for.
    pub requested: DiskSize,
    /// The size the volume has, and keeps.
    pub actual: DiskSize,
}

/// What existed under the sandbox's name before the create, so an abort leaves it alone.
#[derive(Debug, Clone, Copy)]
pub(super) struct Before {
    pub(super) record: bool,
    pub(super) stale_dir: bool,
}

impl Before {
    /// Neither existed.
    pub(super) const NOTHING: Self = Self {
        record: false,
        stale_dir: false,
    };
}

impl Attachment<'_> {
    /// The workspace.
    #[must_use]
    pub fn workspace(&self) -> &WorkspaceId {
        &self.reservation.id
    }

    /// The volume mount to add to the sandbox's spec.
    #[must_use]
    pub fn mount(&self) -> &VolumeMount {
        &self.mount
    }

    /// Whether [`Workspaces::prepare`] created the volume (an abort deletes it again).
    #[must_use]
    pub fn created_volume(&self) -> bool {
        self.created_volume
    }

    /// Set when the volume existed with another size than the one asked for: the request was not
    /// applied, and the caller should say so.
    #[must_use]
    pub fn kept_size(&self) -> Option<KeptSize> {
        self.kept_size
    }

    /// `spec` with the workspace's volume mount added.
    #[must_use]
    pub fn add_to(&self, spec: SandboxSpec) -> SandboxSpec {
        spec.with_volume(self.mount().clone())
    }

    /// The sandbox was created: it holds the workspace from now on.
    pub fn commit(mut self) {
        self.reservation.open = false;
        self.reservation
            .workspaces
            .registry
            .attach(&self.reservation.id, &self.reservation.sandbox);
        info!(workspace = %self.reservation.id, sandbox = %self.reservation.sandbox, "workspace attached");
    }

    /// The create failed: removes what it left (a down sandbox record, a stale directory,
    /// unless they were there before) and the volume if [`Workspaces::prepare`] created it, then
    /// releases the reservation.
    ///
    /// # Errors
    ///
    /// [`WorkspaceError::Runtime`] with the first cleanup step that failed (the others still ran).
    pub async fn abort<R: Runtime>(mut self, rt: &R) -> Result<(), WorkspaceError> {
        self.reservation.open = false;
        let mut result = cleanup_failed_create(rt, &self.reservation.sandbox, self.before)
            .await
            .map_err(|e| {
                WorkspaceError::runtime("clean up after a failed create", &self.reservation.id, e)
            });
        if self.created_volume {
            match rt.remove_volume(&self.reservation.id.volume_name()).await {
                Ok(()) | Err(ComputeError::VolumeNotFound { .. }) => {}
                Err(e) => {
                    result = result.and_then(|()| {
                        Err(WorkspaceError::runtime(
                            "remove the new volume",
                            &self.reservation.id,
                            e,
                        ))
                    });
                }
            }
        }
        self.reservation
            .workspaces
            .registry
            .release(&self.reservation.id, &self.reservation.sandbox);
        result
    }
}

impl Drop for Attachment<'_> {
    fn drop(&mut self) {
        if self.reservation.open {
            warn!(workspace = %self.reservation.id, sandbox = %self.reservation.sandbox, "attachment dropped without commit or abort");
        }
    }
}

impl Workspaces {
    /// Gets workspace `id` ready to attach to the sandbox about to be created as `sandbox`:
    /// checks no other sandbox holds it, reserves it, and creates its volume (`size`, or
    /// [`WorkspaceConfig::default_size`]) if it has none yet. An existing volume keeps its size.
    ///
    /// Add [`Attachment::mount`] to the spec, create the sandbox (with [`Runtime::create`] or
    /// the boot hook), then [`Attachment::commit`] on success or [`Attachment::abort`] on
    /// failure. [`Workspaces::create`] does all of that for a plain create.
    ///
    /// # Errors
    ///
    /// [`WorkspaceError::InUse`] naming the holder; [`WorkspaceError::Runtime`].
    pub async fn prepare<R: Runtime>(
        &self,
        rt: &R,
        id: &WorkspaceId,
        sandbox: &SandboxName,
        size: Option<DiskSize>,
    ) -> Result<Attachment<'_>, WorkspaceError> {
        self.prepare_with(rt, id, sandbox, size, true).await
    }

    /// Like [`Workspaces::prepare`], for a workspace that already has data: a missing volume is
    /// [`WorkspaceError::VolumeMissing`] instead of a new empty one, so a restart can never
    /// quietly put an empty workspace in place of one that lost its volume.
    ///
    /// # Errors
    ///
    /// [`WorkspaceError::VolumeMissing`]; [`WorkspaceError::InUse`]; [`WorkspaceError::Runtime`].
    pub async fn prepare_existing<R: Runtime>(
        &self,
        rt: &R,
        id: &WorkspaceId,
        sandbox: &SandboxName,
    ) -> Result<Attachment<'_>, WorkspaceError> {
        self.prepare_with(rt, id, sandbox, None, false).await
    }

    pub(super) async fn prepare_with<R: Runtime>(
        &self,
        rt: &R,
        id: &WorkspaceId,
        sandbox: &SandboxName,
        size: Option<DiskSize>,
        may_create: bool,
    ) -> Result<Attachment<'_>, WorkspaceError> {
        let was_attached = self.reserve(rt, id, sandbox).await?;
        // On error the reservation drops here, which releases it.
        let reservation = Reservation {
            workspaces: self,
            id: id.clone(),
            sandbox: sandbox.clone(),
            open: true,
            was_attached,
        };
        let mut created_volume = false;
        let mut kept_size = None;
        let volume = id.volume_name();
        let existing = rt
            .volume(&volume)
            .await
            .map_err(|e| WorkspaceError::runtime("look up the volume", id, e))?;
        if let Some(holder) = existing.as_ref().and_then(|v| v.holder.as_ref())
            && holder != sandbox.as_str()
        {
            return Err(WorkspaceError::InUse {
                workspace: id.to_string(),
                holder: holder.clone(),
            });
        }
        let sandboxes = rt
            .list()
            .await
            .map_err(|e| WorkspaceError::runtime("list sandboxes", id, e))?;
        let record = sandboxes.iter().any(|s| s.name == sandbox.as_str());
        let stale = rt
            .stale_dirs()
            .await
            .map_err(|e| WorkspaceError::runtime("list stale directories", id, e))?;
        let stale_dir = stale.iter().any(|s| s == sandbox.as_str());
        let info = if let Some(info) = existing {
            if let Some(requested) = size.filter(|s| *s != info.size) {
                info!(workspace = %id, size = %info.size, "workspace volume exists; keeping its size");
                kept_size = Some(KeptSize {
                    requested,
                    actual: info.size,
                });
            }
            info
        } else if !may_create {
            return Err(WorkspaceError::VolumeMissing {
                workspace: id.to_string(),
                volume: volume.to_string(),
            });
        } else {
            let size = size.unwrap_or(self.config.default_size);
            let info = rt
                .create_volume(VolumeSpec {
                    name: volume.clone(),
                    size,
                })
                .await
                .map_err(|e| WorkspaceError::runtime("create the volume", id, e))?;
            created_volume = true;
            info!(workspace = %id, %size, "workspace volume created");
            info
        };
        Ok(Attachment {
            reservation,
            mount: mount_for(&volume, &info)?,
            created_volume,
            kept_size,
            before: Before { record, stale_dir },
        })
    }

    /// Reserves `id` for `sandbox`, dropping a registry holder that no longer exists.
    pub(super) async fn reserve<R: Runtime>(
        &self,
        rt: &R,
        id: &WorkspaceId,
        sandbox: &SandboxName,
    ) -> Result<bool, WorkspaceError> {
        let in_use = |h: Holder| WorkspaceError::InUse {
            workspace: id.to_string(),
            holder: h.sandbox.to_string(),
        };
        match self.registry.reserve(id, sandbox) {
            Ok(was_attached) => Ok(was_attached),
            Err(h) if h.kind == HoldKind::Attached => {
                let exists = rt
                    .list()
                    .await
                    .map_err(|e| WorkspaceError::runtime("list sandboxes", id, e))?
                    .iter()
                    .any(|s| s.name == h.sandbox.as_str());
                if exists {
                    return Err(in_use(h));
                }
                debug!(workspace = %id, holder = %h.sandbox, "holder is gone; releasing it");
                self.registry.release(id, &h.sandbox);
                self.registry.reserve(id, sandbox).map_err(in_use)
            }
            Err(h) => Err(in_use(h)),
        }
    }

    /// Creates sandbox `spec` with workspace `id` attached ([`Workspaces::prepare`] +
    /// [`Runtime::create`]). On failure nothing is left: no sandbox record, no stale directory,
    /// and no volume if this call created it.
    ///
    /// # Errors
    ///
    /// [`WorkspaceError::InUse`]; [`WorkspaceError::Runtime`] (the create's error; a cleanup
    /// failure after it is logged).
    pub async fn create<R: Runtime>(
        &self,
        rt: &R,
        id: &WorkspaceId,
        spec: SandboxSpec,
        size: Option<DiskSize>,
    ) -> Result<R::Sandbox, WorkspaceError> {
        let attachment = self.prepare(rt, id, &spec.name, size).await?;
        let spec = attachment.add_to(spec);
        match rt.create(spec).await {
            Ok(sandbox) => {
                attachment.commit();
                Ok(sandbox)
            }
            Err(e) => {
                if let Err(cleanup) = attachment.abort(rt).await {
                    warn!(workspace = %id, error = %cleanup, "cleanup after a failed create failed");
                }
                Err(WorkspaceError::runtime("create the sandbox", id, e))
            }
        }
    }
}
