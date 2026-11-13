// SPDX-License-Identifier: AGPL-3.0-or-later
//! Puddle's one kill-on-close job, held for the life of the process.

use std::sync::{Mutex, OnceLock, PoisonError};

use windows_sys::Win32::System::JobObjects::{
    JOB_OBJECT_LIMIT, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};

use super::sys::Job;
use crate::LifecycleError;

/// The job's limits. Breakaway is not allowed, so nothing Puddle starts can leave the job.
/// Later hardening adds its limits here.
const LIMITS: JOB_OBJECT_LIMIT = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;

/// The job, once created. Never dropped: closing the last handle kills this process too.
static JOB: OnceLock<Job> = OnceLock::new();
static CREATE: Mutex<()> = Mutex::new(());

fn job_error(op: &'static str) -> impl FnOnce(std::io::Error) -> LifecycleError {
    move |source| LifecycleError::Job { op, source }
}

pub(crate) fn contain_this_process() -> Result<(), LifecycleError> {
    let _creating = CREATE.lock().unwrap_or_else(PoisonError::into_inner);
    if JOB.get().is_some() {
        return Ok(());
    }
    let job = Job::create(LIMITS).map_err(job_error("create"))?;
    job.assign_current_process()
        .map_err(job_error("assign this process"))?;
    // Nested jobs (Windows 8+) let this work inside a CI runner's or a terminal's own job.
    if !job.contains_current_process().map_err(job_error("check"))? {
        return Err(LifecycleError::Job {
            op: "check",
            source: std::io::Error::other("this process is not in the job after joining it"),
        });
    }
    let _ = JOB.set(job); // Cannot fail: checked above under the lock.
    tracing::debug!("process contained in the kill-on-close job");
    Ok(())
}

pub(crate) fn release_kill_on_close() -> Result<(), LifecycleError> {
    match JOB.get() {
        Some(job) => job
            .set_limits(LIMITS & !JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE)
            .map_err(job_error("lift kill-on-close")),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests;
