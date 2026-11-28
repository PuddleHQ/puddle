// SPDX-License-Identifier: AGPL-3.0-or-later
//! `puddle audit status|prune`: what the activity log holds, and deleting its old records by hand
//! while the host runs. Like the account commands, it works on the database file and never takes
//! the data-folder lock.

use puddle_store::{Actor, NewWorkspaceOp, WorkspaceOpKind, WorkspaceOpOutcome};

use crate::HostPaths;
use crate::accounts::{AccountError, open};

/// What `puddle audit prune` did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuditPruned {
    /// The days the records were kept for.
    pub days: u32,
    /// Records deleted for being older.
    pub deleted: u64,
    /// Bytes the database file gave back to the disk.
    pub shrunk_bytes: u64,
}

/// What the audit holds and how it grows, for `puddle audit status`.
///
/// # Errors
///
/// [`AccountError`] as its variants say.
pub fn audit_status(paths: &HostPaths) -> Result<puddle_store::AuditStats, AccountError> {
    let (store, _, _) = open(paths)?;
    Ok(store.audit_stats()?)
}

/// Deletes the audit records older than `days` (the saved retention when `None`) and gives the
/// file's free space back, now, while the host runs. The records are gone for good.
///
/// # Errors
///
/// [`AccountError`] as its variants say; days outside 1 to 3650 are
/// [`puddle_store::StoreError::AuditRetention`].
pub fn prune_audit(paths: &HostPaths, days: Option<u32>) -> Result<AuditPruned, AccountError> {
    let (store, _, _) = open(paths)?;
    let days = match days {
        Some(days) if (1..=puddle_store::MAX_AUDIT_RETENTION_DAYS).contains(&days) => days,
        Some(days) => return Err(puddle_store::StoreError::AuditRetention(days).into()),
        None => store.audit_retention_days()?,
    };
    let deleted = store.prune_audit(days)?.deleted;
    let shrunk_bytes = store.shrink_audit_file()?;
    // After the delete, so the record that says who asked is never among the records deleted.
    store.note_workspace_op(&NewWorkspaceOp {
        op: WorkspaceOpKind::AuditPruned,
        workspace: None,
        user: None,
        outcome: WorkspaceOpOutcome::Succeeded,
        actor: Actor::Cli,
        detail: Some(format!(
            "records older than {days} days deleted by hand: {deleted} records"
        )),
    });
    Ok(AuditPruned {
        days,
        deleted,
        shrunk_bytes,
    })
}

#[cfg(test)]
mod tests;
