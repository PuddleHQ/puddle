// SPDX-License-Identifier: AGPL-3.0-or-later
//! The admission rules of the workspace operations that do not depend on the workspace's
//! phase (those are [`super::phase`]'s), and the text of each refusal.
//!
//! The real service and the in-memory one both call these, so a rule change reaches both and
//! the API tests that run on the in-memory one cannot drift from the host.

use puddle_types::WorkspaceId;

use super::{DeleteCheck, WorkspaceError, WorkspaceRecord};

/// The refusal for an id no workspace has.
#[must_use]
pub fn not_found(id: &WorkspaceId) -> WorkspaceError {
    WorkspaceError::NotFound(format!("no workspace {id}"))
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
