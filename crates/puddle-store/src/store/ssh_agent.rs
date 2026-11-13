// SPDX-License-Identifier: GPL-3.0-or-later
//! The hosts each workspace may use the user's SSH agent for (the filtered agent's switch).
//!
//! A row exists only while the user has the agent on for that host: there is no "off" row and no
//! default, so nothing but the user's own change turns it on. A change tells the running workspace
//! through [`Event::WorkspaceGitChanged`], the event that makes the host rebuild what the
//! workspace's proxy knows, so it applies to the next request on the agent with no restart.

use puddle_types::{Event, WorkspaceId};
use rusqlite::{OptionalExtension, Transaction, params};

use super::{Store, lock};
use crate::error::StoreError;

/// Most hosts one workspace may have the agent on for.
pub const MAX_SSH_AGENT_HOSTS: usize = 100;

/// Most keys one host may be limited to.
pub const MAX_SSH_AGENT_KEYS: usize = 64;

/// One host a workspace may use the user's SSH agent for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshAgentHost {
    /// The host, as the guest's `ssh` names it (a normalised name or an IP literal).
    pub host: String,
    /// The keys the host may use, as SHA-256 fingerprints (`SHA256:...`) in sorted order; `None`
    /// for every key the agent holds.
    pub keys: Option<Vec<String>>,
    /// Epoch ms the agent was turned on for the host, or its keys last changed.
    pub changed_at: u64,
}

/// Fingerprints are stored one per line: they are base64 and never hold one.
fn joined(keys: Option<&[String]>) -> Option<String> {
    keys.map(|keys| {
        let mut sorted: Vec<&str> = keys.iter().map(String::as_str).collect();
        sorted.sort_unstable();
        sorted.dedup();
        sorted.join("\n")
    })
}

fn split(stored: Option<String>) -> Option<Vec<String>> {
    stored.map(|text| text.lines().map(str::to_owned).collect())
}

fn changed(fx: &mut Vec<Event>, workspace: &WorkspaceId) {
    fx.push(Event::WorkspaceGitChanged {
        workspace: workspace.clone(),
    });
}

impl Store {
    /// The hosts `workspace` may use the agent for, by host name.
    ///
    /// # Errors
    /// A database error.
    pub fn ssh_agent_hosts(
        &self,
        workspace: &WorkspaceId,
    ) -> Result<Vec<SshAgentHost>, StoreError> {
        let conn = lock(&self.conn);
        let mut stmt = conn.prepare(&format!("{COLUMNS} WHERE workspace_id = ?1 ORDER BY host"))?;
        let rows = stmt
            .query_map([workspace.as_str()], read)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Turns the agent on for `host` in `workspace`, for `keys` (fingerprints; `None` = every key),
    /// or changes the keys of a host it is on for. The caller has checked the host and the keys.
    ///
    /// # Errors
    /// [`StoreError::TooManySshAgentHosts`] for a new host past [`MAX_SSH_AGENT_HOSTS`], or a database
    /// error; nothing changes then.
    pub fn set_ssh_agent_host(
        &self,
        workspace: &WorkspaceId,
        host: &str,
        keys: Option<&[String]>,
    ) -> Result<SshAgentHost, StoreError> {
        let keys = joined(keys);
        self.change(|tx, now, fx| {
            let before: Option<Option<String>> = tx
                .query_row(
                    "SELECT keys FROM ssh_agent_hosts WHERE workspace_id = ?1 AND host = ?2",
                    params![workspace.as_str(), host],
                    |r| r.get(0),
                )
                .optional()?;
            if before.is_none() {
                let count = tx.query_row::<i64, _, _>(
                    "SELECT count(*) FROM ssh_agent_hosts WHERE workspace_id = ?1",
                    [workspace.as_str()],
                    |r| r.get(0),
                );
                if usize::try_from(count?).unwrap_or(usize::MAX) >= MAX_SSH_AGENT_HOSTS {
                    return Err(StoreError::TooManySshAgentHosts(MAX_SSH_AGENT_HOSTS));
                }
            }
            if before.as_ref() != Some(&keys) {
                tx.execute(
                    "INSERT INTO ssh_agent_hosts (workspace_id, host, keys, changed_at)
                     VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT (workspace_id, host) DO UPDATE SET keys = ?3, changed_at = ?4",
                    params![
                        workspace.as_str(),
                        host,
                        keys,
                        i64::try_from(now).unwrap_or(i64::MAX)
                    ],
                )?;
                changed(fx, workspace);
            }
            row(tx, workspace, host)
        })
    }

    /// Turns the agent off for `host` in `workspace`. `false` when it was not on.
    ///
    /// # Errors
    /// A database error; nothing changes then.
    pub fn remove_ssh_agent_host(
        &self,
        workspace: &WorkspaceId,
        host: &str,
    ) -> Result<bool, StoreError> {
        self.change(|tx, _now, fx| {
            let removed = tx.execute(
                "DELETE FROM ssh_agent_hosts WHERE workspace_id = ?1 AND host = ?2",
                params![workspace.as_str(), host],
            )? > 0;
            if removed {
                changed(fx, workspace);
            }
            Ok(removed)
        })
    }
}

const COLUMNS: &str = "SELECT host, keys, changed_at FROM ssh_agent_hosts";

fn read(r: &rusqlite::Row<'_>) -> rusqlite::Result<SshAgentHost> {
    Ok(SshAgentHost {
        host: r.get(0)?,
        keys: split(r.get(1)?),
        changed_at: u64::try_from(r.get::<_, i64>(2)?).unwrap_or(0),
    })
}

fn row(
    tx: &Transaction<'_>,
    workspace: &WorkspaceId,
    host: &str,
) -> Result<SshAgentHost, StoreError> {
    Ok(tx.query_row(
        &format!("{COLUMNS} WHERE workspace_id = ?1 AND host = ?2"),
        params![workspace.as_str(), host],
        read,
    )?)
}

/// Removes what a deleted workspace held. The workspace's own change event covers it.
pub(super) fn delete_workspace_rows(
    tx: &Transaction<'_>,
    workspace: &WorkspaceId,
) -> Result<(), StoreError> {
    tx.execute(
        "DELETE FROM ssh_agent_hosts WHERE workspace_id = ?1",
        [workspace.as_str()],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests;
