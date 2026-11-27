// SPDX-License-Identifier: AGPL-3.0-or-later
//! Deleting a workspace's rows and the name each workspace had when an audit record was written.

use puddle_types::{Event, WorkspaceId};
use rusqlite::{OptionalExtension, Transaction, params};

use crate::audit::{AuditRecord, PendingExpiryReason, RuleDeleteReason, RuleWire, actor_str};
use crate::error::StoreError;
use crate::rule::Actor;

use super::rows::{RULE_COLUMNS, expire_rows, raw_rule, rule_from_raw};
use super::{Store, WorkspaceDeletion, append, lock};

impl Store {
    /// Removes a deleted workspace's rules and environment and expires its open rows, in one
    /// transaction (R-21).
    /// Call it from the transaction-equivalent step of workspace deletion, not on stop.
    ///
    /// # Errors
    /// A database or audit error; nothing changes then.
    pub fn delete_workspace(
        &self,
        workspace: &WorkspaceId,
    ) -> Result<WorkspaceDeletion, StoreError> {
        let deletion = self.change(|tx, now, fx| {
            let mut stmt = tx.prepare(&format!(
                "SELECT {RULE_COLUMNS} FROM rules WHERE workspace_id = ?1 ORDER BY id"
            ))?;
            let rules = stmt
                .query_map([workspace.as_str()], raw_rule)?
                .map(|raw| rule_from_raw(&raw?))
                .collect::<Result<Vec<_>, _>>()?;
            drop(stmt);
            tx.execute(
                "DELETE FROM rules WHERE workspace_id = ?1",
                [workspace.as_str()],
            )?;
            for rule in &rules {
                append(
                    tx,
                    &AuditRecord::RuleDeleted {
                        ts: now,
                        rule: RuleWire::from(rule),
                        reason: RuleDeleteReason::WorkspaceDeleted,
                        actor: actor_str(Actor::System),
                    },
                )?;
            }
            let switched = tx.execute(
                "DELETE FROM rule_set_switches WHERE workspace_id = ?1",
                [workspace.as_str()],
            )? + tx.execute(
                "DELETE FROM system_reasons WHERE workspace_id = ?1",
                [workspace.as_str()],
            )?;
            let expired = expire_rows(
                tx,
                "workspace_id = ?2",
                &workspace.as_str(),
                now,
                PendingExpiryReason::WorkspaceDeleted,
                fx,
            )?;
            if !rules.is_empty() || switched > 0 {
                fx.push(Event::RulesChanged {});
            }
            // Its identities and repository table go with it; the identities stay.
            if delete_workspace_rows(tx, workspace)? {
                fx.push(Event::WorkspaceGitChanged {
                    workspace: workspace.clone(),
                });
            }
            // Its variables and stand-ins go with it; the global ones stay.
            let (secrets, had_env) = super::environment::delete_workspace_rows(tx, workspace)?;
            if had_env {
                fx.push(Event::WorkspaceEnvChanged {
                    workspace: workspace.clone(),
                });
            }
            super::shell_init::delete_workspace_rows(tx, workspace)?;
            // Its pinned forwards go with it.
            super::ports::delete_workspace_rows(tx, workspace)?;
            super::interception::delete_workspace_rows(tx, workspace)?;
            super::learned::delete_workspace_rows(tx, workspace)?;
            super::request_rules::delete_workspace_rows(tx, workspace, now, fx)?;
            super::ssh_agent::delete_workspace_rows(tx, workspace)?;
            super::ssh_agent_asks::delete_workspace_rows(tx, workspace)?;
            Ok(WorkspaceDeletion {
                rules_deleted: rules.len() as u64,
                pending_expired: expired,
                secrets,
            })
        })?;
        lock(&self.workspaces).remove(workspace);
        tracing::info!(workspace = %workspace, rules = deletion.rules_deleted, "workspace rules removed");
        Ok(deletion)
    }

    /// Remembers `name` as the current name of `workspace`; audit records written from now on
    /// carry it as `workspace_name`. Call it when a workspace is created or renamed, and for
    /// every workspace at start.
    ///
    /// # Errors
    ///
    /// Fails if the database can't be written.
    pub fn set_workspace_name(
        &self,
        workspace: &WorkspaceId,
        name: &str,
    ) -> Result<(), StoreError> {
        let conn = lock(&self.conn);
        conn.execute(
            "INSERT INTO workspace_names (workspace_id, name) VALUES (?1, ?2)
             ON CONFLICT (workspace_id) DO UPDATE SET name = excluded.name",
            params![workspace.as_str(), name],
        )?;
        Ok(())
    }

    /// The current name of `workspace` as last set with [`Store::set_workspace_name`], or `None`
    /// when none was set or the database can't be read.
    pub fn current_workspace_name(&self, workspace: &WorkspaceId) -> Option<String> {
        let conn = self.audit_read();
        conn.query_row(
            "SELECT name FROM workspace_names WHERE workspace_id = ?1",
            [workspace.as_str()],
            |row| row.get(0),
        )
        .optional()
        .unwrap_or_else(|err| {
            tracing::warn!(%workspace, error = %err, "the workspace's name could not be read");
            None
        })
    }

    /// [`Store::set_workspace_name`] for a caller that cannot act on a failure: it is logged and
    /// dropped, and the records written meanwhile carry no name (a reader shows the id). Returns
    /// whether the name was written.
    pub fn note_workspace_name(&self, workspace: &WorkspaceId, name: &str) -> bool {
        let written = self.set_workspace_name(workspace, name);
        if let Err(err) = &written {
            tracing::warn!(%workspace, error = %err, "the workspace's name was not written to the store");
        }
        written.is_ok()
    }
}

/// Drops what a deleted workspace held in the Git tables.
pub(super) fn delete_workspace_rows(
    tx: &Transaction<'_>,
    ws: &WorkspaceId,
) -> Result<bool, StoreError> {
    let mut any = 0;
    for table in ["workspace_identities", "workspace_git", "workspace_repos"] {
        any += tx.execute(
            &format!("DELETE FROM {table} WHERE workspace_id = ?1"),
            [ws.as_str()],
        )?;
    }
    Ok(any > 0)
}
