// SPDX-License-Identifier: GPL-3.0-or-later
//! Pinned port forwards in the store: a port in a workspace that is forwarded to the same port of
//! this computer each time the workspace starts. What is forwarded right now, and which ports a
//! workspace listens on, is not stored: the host learns it from the guest.

use puddle_types::WorkspaceName;
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
    workspace: &WorkspaceName,
) -> Result<(), StoreError> {
    tx.execute(
        "DELETE FROM pinned_ports WHERE workspace_id = ?1",
        [workspace.as_str()],
    )?;
    Ok(())
}

fn list(conn: &Connection, workspace: &WorkspaceName) -> Result<Vec<PinnedPort>, StoreError> {
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
    pub fn pinned_ports(&self, workspace: &WorkspaceName) -> Result<Vec<PinnedPort>, StoreError> {
        list(&lock(&self.conn), workspace)
    }

    /// Every workspace's pinned forwards, to tell whether a port on this computer is spoken for.
    ///
    /// # Errors
    ///
    /// A database error, or a stored row that is not a workspace name or a port.
    pub fn all_pinned_ports(&self) -> Result<Vec<(WorkspaceName, PinnedPort)>, StoreError> {
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
                let name = WorkspaceName::new(&workspace).map_err(|e| StoreError::Corrupt {
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
    pub fn pin_port(&self, workspace: &WorkspaceName, pin: PinnedPort) -> Result<(), StoreError> {
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
    pub fn unpin_port(
        &self,
        workspace: &WorkspaceName,
        guest_port: u16,
    ) -> Result<bool, StoreError> {
        let removed = lock(&self.conn).execute(
            "DELETE FROM pinned_ports WHERE workspace_id = ?1 AND guest_port = ?2",
            params![workspace.as_str(), guest_port],
        )?;
        Ok(removed > 0)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::clock::ManualClock;
    use crate::store::Limits;

    fn store() -> Store {
        Store::open_in_memory(Arc::new(ManualClock::new(1)), Limits::default()).unwrap()
    }

    fn ws(text: &str) -> WorkspaceName {
        WorkspaceName::new(text).unwrap()
    }

    fn pin(guest_port: u16, host_port: u16) -> PinnedPort {
        PinnedPort {
            guest_port,
            host_port,
        }
    }

    #[test]
    fn a_pin_is_kept_replaced_and_removed_per_workspace() {
        let store = store();
        let (shop, blog) = (ws("shop"), ws("blog"));
        store.pin_port(&shop, pin(8080, 18080)).unwrap();
        store.pin_port(&shop, pin(3000, 3000)).unwrap();
        store.pin_port(&blog, pin(3000, 3001)).unwrap();
        assert_eq!(
            store.pinned_ports(&shop).unwrap(),
            [pin(3000, 3000), pin(8080, 18080)]
        );
        // The same guest port pinned again moves to the new host port; it does not double.
        store.pin_port(&shop, pin(3000, 4000)).unwrap();
        assert_eq!(
            store.pinned_ports(&shop).unwrap(),
            [pin(3000, 4000), pin(8080, 18080)]
        );
        assert_eq!(store.pinned_ports(&blog).unwrap(), [pin(3000, 3001)]);
        assert_eq!(
            store.all_pinned_ports().unwrap(),
            [
                (blog.clone(), pin(3000, 3001)),
                (shop.clone(), pin(3000, 4000)),
                (shop.clone(), pin(8080, 18080)),
            ]
        );
        assert!(store.unpin_port(&shop, 3000).unwrap());
        assert!(!store.unpin_port(&shop, 3000).unwrap());
        assert_eq!(store.pinned_ports(&shop).unwrap(), [pin(8080, 18080)]);
    }

    #[test]
    fn a_port_the_table_refuses_changes_nothing() {
        let store = store();
        let shop = ws("shop");
        store.pin_port(&shop, pin(3000, 3000)).unwrap();
        assert!(store.pin_port(&shop, pin(0, 3000)).is_err());
        assert!(store.pin_port(&shop, pin(3000, 0)).is_err());
        assert_eq!(store.pinned_ports(&shop).unwrap(), [pin(3000, 3000)]);
    }

    #[test]
    fn a_row_that_is_not_a_port_or_a_workspace_is_reported_not_trusted() {
        let store = store();
        // Written behind the table's back: the checks refuse these, so go around them.
        {
            let conn = lock(&store.conn);
            conn.execute_batch(
                "PRAGMA ignore_check_constraints = ON;
                 INSERT INTO pinned_ports VALUES ('shop', 70000, 80);
                 INSERT INTO pinned_ports VALUES ('Not A Name', 80, 80);",
            )
            .unwrap();
        }
        assert!(matches!(
            store.pinned_ports(&ws("shop")),
            Err(StoreError::Corrupt {
                table: "pinned_ports",
                ..
            })
        ));
        assert!(matches!(
            store.all_pinned_ports(),
            Err(StoreError::Corrupt {
                table: "pinned_ports",
                ..
            })
        ));
    }

    #[test]
    fn deleting_a_workspace_removes_its_pins_and_only_its_own() {
        let store = store();
        let (shop, blog) = (ws("shop"), ws("blog"));
        store.pin_port(&shop, pin(3000, 3000)).unwrap();
        store.pin_port(&blog, pin(3000, 3001)).unwrap();
        store.delete_workspace(&shop).unwrap();
        assert_eq!(store.pinned_ports(&shop).unwrap(), []);
        assert_eq!(store.pinned_ports(&blog).unwrap(), [pin(3000, 3001)]);
    }
}
