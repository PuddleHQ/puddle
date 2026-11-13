// SPDX-License-Identifier: AGPL-3.0-or-later
//! Shell init rows in the store.

use puddle_types::{Event, WorkspaceId};
use rusqlite::{OptionalExtension, Transaction, params};

use super::{Store, lock, sql_ts};
use crate::error::StoreError;
use crate::shell_init::{ShellInit, ShellInitText};

fn changed(fx: &mut Vec<Event>, workspace: &WorkspaceId) {
    fx.push(Event::WorkspaceShellInitChanged {
        workspace: workspace.clone(),
    });
}

impl Store {
    /// The shell init saved for `workspace`, if it has one.
    ///
    /// # Errors
    /// A database error, or a stored row that does not parse.
    pub fn shell_init(&self, workspace: &WorkspaceId) -> Result<Option<ShellInit>, StoreError> {
        let conn = lock(&self.conn);
        let row: Option<(String, i64)> = conn
            .query_row(
                "SELECT content, changed_at FROM shell_init WHERE workspace_id = ?1",
                [workspace.as_str()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        row.map(|(content, changed_at)| {
            Ok(ShellInit {
                content: ShellInitText::new(&content).map_err(|e| StoreError::Corrupt {
                    table: "shell_init",
                    id: 0,
                    reason: e.to_string(),
                })?,
                changed_at: u64::try_from(changed_at).unwrap_or(0),
            })
        })
        .transpose()
    }

    /// Saves `content` as `workspace`'s shell init, replacing what was there. An unchanged text
    /// still counts as a save (it carries a new time) but sends no event.
    ///
    /// # Errors
    /// A database error; nothing changes then.
    pub fn set_shell_init(
        &self,
        workspace: &WorkspaceId,
        content: &ShellInitText,
    ) -> Result<ShellInit, StoreError> {
        let before = self.shell_init(workspace)?;
        let unchanged = before.as_ref().is_some_and(|b| &b.content == content);
        let changed_at = self.change(|tx, now, fx| {
            tx.execute(
                "INSERT INTO shell_init (workspace_id, content, changed_at) VALUES (?1, ?2, ?3)
                 ON CONFLICT (workspace_id) DO UPDATE SET content = ?2, changed_at = ?3",
                params![workspace.as_str(), content.as_str(), sql_ts(now)],
            )?;
            if !unchanged {
                changed(fx, workspace);
            }
            Ok(now)
        })?;
        Ok(ShellInit {
            content: content.clone(),
            changed_at,
        })
    }

    /// Removes `workspace`'s shell init. Returns whether it had one.
    ///
    /// # Errors
    /// A database error; nothing changes then.
    pub fn clear_shell_init(&self, workspace: &WorkspaceId) -> Result<bool, StoreError> {
        self.change(|tx, _now, fx| {
            let had = tx.execute(
                "DELETE FROM shell_init WHERE workspace_id = ?1",
                [workspace.as_str()],
            )? > 0;
            if had {
                changed(fx, workspace);
            }
            Ok(had)
        })
    }
}

/// Removes what a deleted workspace held. The workspace's own change event covers it (its page is
/// gone), so none is sent here.
pub(super) fn delete_workspace_rows(
    tx: &Transaction<'_>,
    workspace: &WorkspaceId,
) -> Result<(), StoreError> {
    tx.execute(
        "DELETE FROM shell_init WHERE workspace_id = ?1",
        [workspace.as_str()],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests;
