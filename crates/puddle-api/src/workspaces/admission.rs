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
mod tests {
    use super::*;
    use crate::workspaces::{DisplayName, Listing, Operation};

    fn record(status: WorkspaceStatus) -> WorkspaceRecord {
        let mut record = WorkspaceRecord::new(
            WorkspaceId::new("web").unwrap(),
            DisplayName::parse("web").unwrap(),
            Vec::new(),
        );
        record.status = status;
        record
    }

    fn says(result: Result<(), WorkspaceError>, words: &str) {
        match result {
            Err(WorkspaceError::Conflict(m) | WorkspaceError::Invalid(m)) => {
                assert!(m.contains(words), "{m}");
            }
            other => panic!("expected a refusal saying {words:?}, got {other:?}"),
        }
    }

    #[test]
    fn each_operation_is_admitted_in_the_states_it_needs_and_refused_in_the_rest() {
        use WorkspaceStatus::{Crashed, Created, Draining, Running, Starting, Stopped};
        for status in [Created, Stopped, Crashed] {
            let r = record(status);
            assert!(admit_start(&r).is_ok());
            assert!(admit_reset_root_disk(&r).is_ok());
            assert!(admit_delete_status(&r).is_ok());
            says(admit_stop(&r), "is not running");
        }
        for status in [Running, Starting, Draining] {
            let r = record(status);
            says(admit_start(&r), "is already");
            says(admit_reset_root_disk(&r), "stop it before resetting");
            says(admit_delete_status(&r), "stop it before deleting");
        }
        assert!(admit_stop(&record(Running)).is_ok());
        says(admit_settled(&record(Starting)), "try again in a moment");
        says(admit_settled(&record(Draining)), "try again in a moment");
        assert!(admit_settled(&record(Running)).is_ok());
    }

    #[test]
    fn a_busy_workspace_takes_no_second_operation() {
        let mut r = record(WorkspaceStatus::Running);
        assert!(admit_idle(&r).is_ok());
        r.busy = Some(Operation::Stopping);
        says(admit_idle(&r), "is busy (stopping)");
    }

    #[test]
    fn a_root_disk_only_grows() {
        assert!(admit_grow_size(5000, 4096).is_ok());
        says(admit_grow_size(4096, 4096), "only grows");
        says(admit_grow_size(100, 4096), "its 4096 MiB");
    }

    #[test]
    fn delete_needs_the_fingerprint_the_check_gave_or_a_clean_check() {
        let r = record(WorkspaceStatus::Stopped);
        let clean = DeleteCheck::new(
            r.id.clone(),
            Vec::new(),
            Listing::default(),
            Vec::new(),
            None,
        );
        assert!(admit_delete_fingerprint(&r, None, &clean).is_ok());
        assert!(admit_delete_fingerprint(&r, Some(&clean.fingerprint), &clean).is_ok());
        says(
            admit_delete_fingerprint(&r, Some("stale"), &clean),
            "has changed since it was checked",
        );
    }

    #[test]
    fn an_unknown_id_is_not_found_and_names_the_id() {
        let id = WorkspaceId::new("ghost").unwrap();
        assert!(matches!(not_found(&id), WorkspaceError::NotFound(m) if m == "no workspace ghost"));
    }
}
