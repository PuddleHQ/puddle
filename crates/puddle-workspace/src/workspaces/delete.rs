// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

impl Workspaces {
    /// Step one of a delete: lists what deleting workspace `id` would lose (uncommitted changes,
    /// unpushed commits, stashes, data outside any checkout), checked in its running holder or
    /// in a short-lived maintenance sandbox. Show it, and pass [`DeleteReport::confirm`] to
    /// [`Workspaces::delete`] if the user agrees. A workspace whose volume is gone has nothing
    /// to inspect: the report says so ([`DeleteReport::volume_missing`]) and a delete drops
    /// only its sandbox record, so the caller should ask only about workspaces it lists.
    ///
    /// # Errors
    ///
    /// [`WorkspaceError::Check`] when the check can't run or its
    /// output can't be read (fail closed); [`WorkspaceError::InUse`] while a create or another
    /// maintenance run has it; [`WorkspaceError::Runtime`].
    pub async fn check_delete<R: Runtime>(
        &self,
        rt: &R,
        id: &WorkspaceId,
    ) -> Result<DeleteReport, WorkspaceError> {
        let Some(info) = self.volume_if_any(rt, id).await? else {
            // The volume is gone (lost or removed outside Puddle): nothing to inspect, nothing
            // to lose. The caller only asks about workspaces it lists.
            return Ok(DeleteReport {
                workspace: id.clone(),
                findings: Findings::default(),
                checked_in: maintenance_name(id)?,
                removes_sandbox: self.existing_owner(rt, id).await?,
                volume_missing: true,
            });
        };
        let removes_sandbox = self.existing_owner(rt, id).await?;
        if let Some(holder) = self.running_holder(id, &info)? {
            let sandbox = connect_to_holder(rt, id, &holder).await?;
            let findings = check::run(&sandbox, id).await?;
            return Ok(DeleteReport {
                workspace: id.clone(),
                findings,
                checked_in: holder,
                removes_sandbox,
                volume_missing: false,
            });
        }
        let checked_in = maintenance_name(id)?;
        match self.maintenance(rt, id, &info, Job::Check).await? {
            JobOutput::Check(findings) => Ok(DeleteReport {
                workspace: id.clone(),
                findings,
                checked_in,
                removes_sandbox,
                volume_missing: false,
            }),
            JobOutput::Trim(_) | JobOutput::Checkouts(_) => unreachable_job(id),
        }
    }

    /// Step two: deletes workspace `id` (its volume and the stopped sandbox that owns it), after
    /// checking again in a short-lived maintenance sandbox that nothing changed since `confirmed`.
    ///
    /// # Errors
    ///
    /// - [`WorkspaceError::WrongConfirmation`]: `confirmed` is for another workspace;
    /// - [`WorkspaceError::InUse`]: a sandbox runs with it (stop it first), or a create or
    ///   maintenance run has it;
    /// - [`WorkspaceError::Changed`]: the check finds something else now (nothing deleted);
    /// - [`WorkspaceError::NotFound`], [`WorkspaceError::Check`], [`WorkspaceError::Runtime`].
    pub async fn delete<R: Runtime>(
        &self,
        rt: &R,
        id: &WorkspaceId,
        confirmed: &DeleteConfirmation,
    ) -> Result<Deleted, WorkspaceError> {
        if confirmed.workspace != *id {
            return Err(WorkspaceError::WrongConfirmation {
                workspace: id.to_string(),
                confirmed: confirmed.workspace.to_string(),
            });
        }
        let Some(info) = self.volume_if_any(rt, id).await? else {
            return self.delete_without_volume(rt, id, confirmed).await;
        };
        if confirmed.volume_missing {
            // It was gone when the user looked and is back now: it may hold work.
            return Err(self.changed(rt, id).await);
        }
        if let Some(holder) = self.running_holder(id, &info)? {
            return Err(WorkspaceError::InUse {
                workspace: id.to_string(),
                holder: holder.to_string(),
            });
        }
        let checked_in = maintenance_name(id)?;
        self.borrow(id, &checked_in)?;
        let loan = Loan {
            registry: &self.registry,
            id,
        };
        // Read under the loan: a create that starts while this awaits is refused, not missed.
        let owner = self.existing_owner(rt, id).await?;
        let result = self
            .delete_borrowed(rt, id, &info, confirmed, owner, checked_in)
            .await;
        if result.is_ok() {
            self.registry.forget(id);
        }
        drop(loan);
        result
    }

    pub(super) async fn delete_borrowed<R: Runtime>(
        &self,
        rt: &R,
        id: &WorkspaceId,
        info: &VolumeInfo,
        confirmed: &DeleteConfirmation,
        owner: Option<SandboxName>,
        checked_in: SandboxName,
    ) -> Result<Deleted, WorkspaceError> {
        let findings = match self.run_maintenance(rt, id, info, Job::Check).await? {
            JobOutput::Check(f) => f,
            JobOutput::Trim(_) | JobOutput::Checkouts(_) => return unreachable_job(id),
        };
        if findings != confirmed.findings || owner != confirmed.removes_sandbox {
            return Err(WorkspaceError::Changed {
                workspace: id.to_string(),
                report: Box::new(DeleteReport {
                    workspace: id.clone(),
                    findings,
                    checked_in,
                    removes_sandbox: owner,
                    volume_missing: false,
                }),
            });
        }
        if let Some(sandbox) = &owner {
            rt.remove(sandbox)
                .await
                .map_err(|e| WorkspaceError::runtime("remove the owning sandbox", id, e))?;
            self.sandbox_removed(sandbox);
        }
        let volume = id.volume_name();
        rt.remove_volume(&volume)
            .await
            .map_err(|e| WorkspaceError::runtime("remove the volume", id, e))?;
        info!(workspace = %id, removed_sandbox = ?owner.as_ref().map(SandboxName::as_str), "workspace deleted");
        Ok(Deleted {
            workspace: id.clone(),
            removed_sandbox: owner,
        })
    }

    /// A delete of a workspace whose volume is gone: only the owning sandbox is left to remove.
    pub(super) async fn delete_without_volume<R: Runtime>(
        &self,
        rt: &R,
        id: &WorkspaceId,
        confirmed: &DeleteConfirmation,
    ) -> Result<Deleted, WorkspaceError> {
        let owner = self.existing_owner(rt, id).await?;
        if !confirmed.volume_missing || owner != confirmed.removes_sandbox {
            return Err(self.changed(rt, id).await);
        }
        if let Some(sandbox) = &owner {
            rt.remove(sandbox)
                .await
                .map_err(|e| WorkspaceError::runtime("remove the owning sandbox", id, e))?;
            self.sandbox_removed(sandbox);
        }
        self.registry.forget(id);
        info!(workspace = %id, volume_missing = true, removed_sandbox = ?owner.as_ref().map(SandboxName::as_str), "workspace deleted; its volume was already gone");
        Ok(Deleted {
            workspace: id.clone(),
            removed_sandbox: owner,
        })
    }

    /// [`WorkspaceError::Changed`] with a fresh report (or the error that stopped the check).
    pub(super) async fn changed<R: Runtime>(&self, rt: &R, id: &WorkspaceId) -> WorkspaceError {
        match self.check_delete(rt, id).await {
            Ok(report) => WorkspaceError::Changed {
                workspace: id.to_string(),
                report: Box::new(report),
            },
            Err(e) => e,
        }
    }
}
