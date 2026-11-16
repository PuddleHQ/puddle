// SPDX-License-Identifier: AGPL-3.0-or-later
//! The admission rules of the workspace operations: which states refuse which call, and the
//! text of each refusal.
//!
//! The real service and the in-memory one both call these, so a rule change reaches both and
//! the API tests that run on the in-memory one cannot drift from the host.

use puddle_types::{WorkspaceId, WorkspaceStatus};

use super::{DeleteCheck, WorkspaceError, WorkspaceRecord};

/// The refusal for an id no workspace has.
#[must_use]
pub fn not_found(id: &WorkspaceId) -> WorkspaceError {
    WorkspaceError::NotFound(format!("no workspace {id}"))
}

/// A workspace in the middle of an operation takes no second one.
///
/// # Errors
///
/// [`WorkspaceError::Conflict`] while `record` is busy.
pub fn admit_idle(record: &WorkspaceRecord) -> Result<(), WorkspaceError> {
    match record.busy {
        Some(busy) => Err(WorkspaceError::Conflict(format!(
            "{} is busy ({busy}); wait for it to finish",
            record.name
        ))),
        None => Ok(()),
    }
}

/// Start needs a workspace that is down.
///
/// # Errors
///
/// [`WorkspaceError::Conflict`] when it is up or on its way.
pub fn admit_start(record: &WorkspaceRecord) -> Result<(), WorkspaceError> {
    if record.status.is_down() {
        Ok(())
    } else {
        Err(WorkspaceError::Conflict(format!(
            "{} is already {}",
            record.name, record.status
        )))
    }
}

/// Stop needs a running workspace.
///
/// # Errors
///
/// [`WorkspaceError::Conflict`] when it is not running.
pub fn admit_stop(record: &WorkspaceRecord) -> Result<(), WorkspaceError> {
    if record.status == WorkspaceStatus::Running {
        Ok(())
    } else {
        Err(WorkspaceError::Conflict(format!(
            "{} is not running ({})",
            record.name, record.status
        )))
    }
}

/// Reclaiming space and growing a root disk wait out a start or a stop in progress.
///
/// # Errors
///
/// [`WorkspaceError::Conflict`] while the workspace is starting or draining.
pub fn admit_settled(record: &WorkspaceRecord) -> Result<(), WorkspaceError> {
    if matches!(
        record.status,
        WorkspaceStatus::Starting | WorkspaceStatus::Draining
    ) {
        Err(WorkspaceError::Conflict(format!(
            "{} is {}; try again in a moment",
            record.name, record.status
        )))
    } else {
        Ok(())
    }
}

/// Resetting the root disk needs a workspace that is down.
///
/// # Errors
///
/// [`WorkspaceError::Conflict`] when it is up or on its way.
pub fn admit_reset_root_disk(record: &WorkspaceRecord) -> Result<(), WorkspaceError> {
    if record.status.is_down() {
        Ok(())
    } else {
        Err(WorkspaceError::Conflict(format!(
            "{} is {}; stop it before resetting its root disk",
            record.name, record.status
        )))
    }
}

/// A root disk only grows: `requested_mib` has to beat `current_mib`.
///
/// # Errors
///
/// [`WorkspaceError::Invalid`] when it does not.
pub fn admit_grow_size(requested_mib: u32, current_mib: u64) -> Result<(), WorkspaceError> {
    if u64::from(requested_mib) <= current_mib {
        Err(WorkspaceError::Invalid(format!(
            "a root disk only grows: {requested_mib} MiB is not above its {current_mib} MiB"
        )))
    } else {
        Ok(())
    }
}

/// Delete needs a workspace that is down.
///
/// # Errors
///
/// [`WorkspaceError::Conflict`] when it is up or on its way.
pub fn admit_delete_status(record: &WorkspaceRecord) -> Result<(), WorkspaceError> {
    if record.status.is_down() {
        Ok(())
    } else {
        Err(WorkspaceError::Conflict(format!(
            "{} is {}; stop it before deleting it",
            record.name, record.status
        )))
    }
}

/// Delete goes ahead when the caller confirmed what a fresh check finds, or when the check is
/// clean and the caller confirmed nothing.
///
/// # Errors
///
/// [`WorkspaceError::Conflict`] when the check changed since the caller saw it, or holds work
/// the caller did not confirm.
pub fn admit_delete_fingerprint(
    record: &WorkspaceRecord,
    fingerprint: Option<&str>,
    fresh: &DeleteCheck,
) -> Result<(), WorkspaceError> {
    match fingerprint {
        Some(seen) if seen == fresh.fingerprint => Ok(()),
        Some(_) => Err(WorkspaceError::Conflict(format!(
            "{} has changed since it was checked; check it again",
            record.name
        ))),
        None if fresh.is_clean() => Ok(()),
        None => Err(WorkspaceError::Conflict(format!(
            "{} has work that is not saved on a remote; review it and confirm",
            record.name
        ))),
    }
}

#[cfg(test)]
mod tests;
