// SPDX-License-Identifier: AGPL-3.0-or-later
//! Pinned port forwards in the store: a port in a workspace that is forwarded to the same port of
//! this computer each time the workspace starts. What is forwarded right now, and which ports a
//! workspace listens on, is not stored: the host learns it from the guest.

use puddle_types::WorkspaceId;
use rusqlite::{Connection, Transaction, params};

use super::{Store, lock};
use crate::error::StoreError;

/// One pinned forward.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct PinnedPort {
    /// The port in the workspace.
    pub guest_port: u16,
    /// The port on this computer.
    pub host_port: u16,
}

fn port(value: i64, column: &str) -> Result<u16, StoreError> {
    u16::try_from(value).map_err(|_| StoreError::Corrupt {
        table: "pinned_ports",
        id: value,
        reason: format!("{column} is not a port"),
    })
}

/// A deleted workspace's pins go with it.
pub(super) fn delete_workspace_rows(
    tx: &Transaction<'_>,
    workspace: &WorkspaceId,
) -> Result<(), StoreError> {
    tx.execute(
        "DELETE FROM pinned_ports WHERE workspace_id = ?1",
        [workspace.as_str()],
    )?;
    Ok(())
}

fn list(conn: &Connection, workspace: &WorkspaceId) -> Result<Vec<PinnedPort>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT guest_port, host_port FROM pinned_ports WHERE workspace_id = ?1 ORDER BY guest_port",
    )?;
    let rows = stmt
        .query_map([workspace.as_str()], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    rows.into_iter()
        .map(|(guest, host)| {
            Ok(PinnedPort {
                guest_port: port(guest, "guest_port")?,
                host_port: port(host, "host_port")?,
            })
        })
        .collect()
}

impl Store {
    /// The forwards `workspace` has pinned, by guest port.
    ///
    /// # Errors
    ///
    /// A database error, or a stored row that is not a port.
    pub fn pinned_ports(&self, workspace: &WorkspaceId) -> Result<Vec<PinnedPort>, StoreError> {
        list(&lock(&self.conn), workspace)
    }

    /// Every workspace's pinned forwards, to tell whether a port on this computer is spoken for.
    ///
    /// # Errors
    ///
    /// A database error, or a stored row that is not a workspace name or a port.
    pub fn all_pinned_ports(&self) -> Result<Vec<(WorkspaceId, PinnedPort)>, StoreError> {
        let conn = lock(&self.conn);
        let mut stmt = conn.prepare(
            "SELECT workspace_id, guest_port, host_port FROM pinned_ports ORDER BY workspace_id, guest_port",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|(workspace, guest, host)| {
                let name = WorkspaceId::new(&workspace).map_err(|e| StoreError::Corrupt {
                    table: "pinned_ports",
                    id: guest,
                    reason: e.to_string(),
                })?;
                Ok((
                    name,
                    PinnedPort {
                        guest_port: port(guest, "guest_port")?,
                        host_port: port(host, "host_port")?,
                    },
                ))
            })
            .collect()
    }

    /// Pins `workspace`'s `guest_port` to `host_port`, replacing an earlier pin of that guest
    /// port. Ports are 1 to 65535; the caller has checked them.
    ///
    /// # Errors
    ///
    /// A database error (including a port the table's checks refuse). Nothing changes then.
    pub fn pin_port(&self, workspace: &WorkspaceId, pin: PinnedPort) -> Result<(), StoreError> {
        lock(&self.conn).execute(
            "INSERT INTO pinned_ports (workspace_id, guest_port, host_port) VALUES (?1, ?2, ?3)
             ON CONFLICT (workspace_id, guest_port) DO UPDATE SET host_port = excluded.host_port",
            params![workspace.as_str(), pin.guest_port, pin.host_port],
        )?;
        Ok(())
    }

    /// Removes `workspace`'s pin on `guest_port`; `true` when there was one.
    ///
    /// # Errors
    ///
    /// A database error.
    pub fn unpin_port(&self, workspace: &WorkspaceId, guest_port: u16) -> Result<bool, StoreError> {
        let removed = lock(&self.conn).execute(
            "DELETE FROM pinned_ports WHERE workspace_id = ?1 AND guest_port = ?2",
            params![workspace.as_str(), guest_port],
        )?;
        Ok(removed > 0)
    }
}

#[cfg(test)]
mod tests;
