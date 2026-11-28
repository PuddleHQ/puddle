// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

/// The work a maintenance sandbox does.
pub(super) enum Job {
    Check,
    Trim,
    Checkouts,
}

pub(super) enum JobOutput {
    Check(Findings),
    Trim(TrimReport),
    Checkouts(CheckoutList),
}

/// A maintenance loan of a workspace, given back when this is dropped: also when the future that
/// holds it is dropped mid-way (a request that timed out), which would otherwise leave the
/// workspace borrowed until the process restarts.
pub(super) struct Loan<'a> {
    pub(super) registry: &'a Registry,
    pub(super) id: &'a WorkspaceId,
}

impl Drop for Loan<'_> {
    fn drop(&mut self) {
        self.registry.give_back(self.id);
    }
}

impl Workspaces {
    /// Runs `job` in a maintenance sandbox, holding the registry loan for its duration.
    pub(super) async fn maintenance<R: Runtime>(
        &self,
        rt: &R,
        id: &WorkspaceId,
        info: &VolumeInfo,
        job: Job,
    ) -> Result<JobOutput, WorkspaceError> {
        let name = maintenance_name(id)?;
        self.borrow(id, &name)?;
        let _loan = Loan {
            registry: &self.registry,
            id,
        };
        self.run_maintenance(rt, id, info, job).await
    }

    /// Creates the maintenance sandbox with the workspace, runs `job`, and stops and removes the
    /// sandbox again, whatever the job's outcome.
    pub(super) async fn run_maintenance<R: Runtime>(
        &self,
        rt: &R,
        id: &WorkspaceId,
        info: &VolumeInfo,
        job: Job,
    ) -> Result<JobOutput, WorkspaceError> {
        let name = maintenance_name(id)?;
        clear_leftover(rt, id, &name).await?;
        let volume = id.volume_name();
        let spec = SandboxSpec::new(name.clone(), self.config.maintenance_image.clone())
            .with_memory(self.config.maintenance_memory)
            .with_cpus(1)
            .with_volume(mount_for(&volume, info)?);
        debug!(workspace = %id, sandbox = %name, "starting a maintenance sandbox");
        let sandbox = match rt.create(spec).await {
            Ok(s) => s,
            Err(e) => {
                if let Err(cleanup) = cleanup_failed_create(rt, &name, Before::NOTHING).await {
                    warn!(workspace = %id, error = %cleanup, "cleanup after a failed maintenance create failed");
                }
                return Err(WorkspaceError::runtime(
                    "create the maintenance sandbox",
                    id,
                    e,
                ));
            }
        };
        let output = match job {
            Job::Check => check::run(&sandbox, id).await.map(JobOutput::Check),
            Job::Trim => self.trim(&sandbox, id).await.map(JobOutput::Trim),
            Job::Checkouts => self
                .list_checkouts(&sandbox, id)
                .await
                .map(JobOutput::Checkouts),
        };
        let stopped = sandbox.stop().await;
        drop(sandbox);
        let removed = rt.remove(&name).await;
        let cleanup = stopped
            .map_err(|e| WorkspaceError::runtime("stop the maintenance sandbox", id, e))
            .and_then(|()| {
                removed
                    .map_err(|e| WorkspaceError::runtime("remove the maintenance sandbox", id, e))
            });
        match (output, cleanup) {
            (Ok(output), Ok(())) => Ok(output),
            (Ok(_), Err(e)) => Err(e),
            (Err(job), Ok(())) => Err(job),
            // Say both: the job's error alone would hide that a sandbox is left behind.
            (Err(job), Err(leftover)) => {
                warn!(workspace = %id, error = %leftover, "the maintenance sandbox was left behind");
                Err(WorkspaceError::Leftover {
                    workspace: id.to_string(),
                    job: job.to_string(),
                    leftover: leftover.to_string(),
                })
            }
        }
    }
}
