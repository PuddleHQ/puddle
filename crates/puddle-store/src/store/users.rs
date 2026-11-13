// SPDX-License-Identifier: GPL-3.0-or-later
//! Users and who owns which workspace. Credentials are not here: see [`crate::AccountSecrets`].

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use puddle_types::WorkspaceId;
use rusqlite::{Connection, OptionalExtension, Row, params};

use super::{Store, corrupt, lock, sql_ts, stored_ts};
use crate::account_secrets::AccountSecrets;
use crate::error::StoreError;
use crate::users::{Role, User, UserId, UserName};

/// A stored role, or the corruption it is.
pub(crate) fn role_of(user: i64, role: &str) -> Result<Role, StoreError> {
    Role::parse(role).ok_or_else(|| corrupt("users", user, "unknown role"))
}

fn user_of(row: &Row<'_>) -> Result<User, StoreError> {
    let id: i64 = row.get(0)?;
    let role: String = row.get(1)?;
    Ok(User {
        id: UserId::from_stored(id),
        role: role_of(id, &role)?,
        name: row.get(2)?,
        created_at: stored_ts("users", id, row.get(3)?)?,
    })
}

/// Every recorded workspace user.
pub(super) fn load_owners(conn: &Connection) -> Result<HashMap<WorkspaceId, UserId>, StoreError> {
    let mut stmt = conn.prepare("SELECT workspace_id, user_id FROM workspace_owners")?;
    let mut rows = stmt.query([])?;
    let mut map = HashMap::new();
    while let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        let workspace =
            WorkspaceId::new(&id).map_err(|e| corrupt("workspace_owners", 0, e.to_string()))?;
        map.insert(workspace, UserId::from_stored(row.get(1)?));
    }
    Ok(map)
}

/// The user `workspace` belongs to, read inside a transaction.
///
/// # Errors
/// [`StoreError::UnknownWorkspace`] when no user is recorded for it: a workspace that is nobody's
/// has no rules to decide by and nothing to write to, so it reads as one that does not exist.
pub(super) fn owner_in(conn: &Connection, workspace: &WorkspaceId) -> Result<UserId, StoreError> {
    let id: Option<i64> = conn
        .query_row(
            "SELECT user_id FROM workspace_owners WHERE workspace_id = ?1",
            [workspace.as_str()],
            |r| r.get(0),
        )
        .optional()?;
    id.map(UserId::from_stored)
        .ok_or_else(|| StoreError::UnknownWorkspace(workspace.clone()))
}

/// Fails with [`StoreError::UnknownWorkspace`] unless `workspace` is `user`'s: another user's
/// workspace reads as one that does not exist.
pub(super) fn require_owner(
    conn: &Connection,
    workspace: &WorkspaceId,
    user: UserId,
) -> Result<(), StoreError> {
    if owner_in(conn, workspace)? == user {
        Ok(())
    } else {
        Err(StoreError::UnknownWorkspace(workspace.clone()))
    }
}

impl Store {
    /// The sign-in secrets, the first time this is called; `None` after that. The host takes it at
    /// start and gives it to sign-in alone.
    #[must_use]
    pub fn take_account_secrets(&self) -> Option<AccountSecrets> {
        if self.secrets_taken.swap(true, Ordering::SeqCst) {
            return None;
        }
        Some(AccountSecrets::new(
            Arc::clone(&self.conn),
            Arc::clone(&self.clock),
        ))
    }

    /// Every user, the owner first, then by id.
    ///
    /// # Errors
    /// A database error, or a stored row that does not parse.
    pub fn users(&self) -> Result<Vec<User>, StoreError> {
        let conn = lock(&self.conn);
        let mut stmt = conn.prepare(
            "SELECT id, role, name, created_at FROM users ORDER BY role = 'owner' DESC, id",
        )?;
        let mut rows = stmt.query([])?;
        let mut users = Vec::new();
        while let Some(row) = rows.next()? {
            users.push(user_of(row)?);
        }
        Ok(users)
    }

    /// The install's reach across every user's data, the first time this is called; `None` after
    /// that. The host takes it at start, before anything serves a request, so no request handler
    /// can hold one.
    #[must_use]
    pub fn take_all_users(&self) -> Option<AllUsers> {
        if self.all_users_taken.swap(true, Ordering::SeqCst) {
            return None;
        }
        Some(AllUsers::new())
    }

    /// One user.
    ///
    /// # Errors
    /// [`StoreError::UnknownUser`], or a database error.
    pub fn user(&self, id: UserId) -> Result<User, StoreError> {
        let conn = lock(&self.conn);
        let mut stmt =
            conn.prepare("SELECT id, role, name, created_at FROM users WHERE id = ?1")?;
        let mut rows = stmt.query([id.get()])?;
        match rows.next()? {
            Some(row) => user_of(row),
            None => Err(StoreError::UnknownUser(id)),
        }
    }

    /// Adds a plain user (never a second owner).
    ///
    /// # Errors
    /// [`StoreError::UserName`] when another user has the name (ignoring ASCII case), or a
    /// database error.
    pub fn add_user(&self, name: &UserName) -> Result<User, StoreError> {
        let conn = lock(&self.conn);
        let now = self.clock.now_ms();
        let taken: bool = conn.query_row(
            "SELECT count(*) > 0 FROM users WHERE name = ?1",
            [name.as_str()],
            |r| r.get(0),
        )?;
        if taken {
            return Err(StoreError::UserName("that user name is taken".to_owned()));
        }
        conn.execute(
            "INSERT INTO users (role, name, created_at) VALUES ('user', ?1, ?2)",
            params![name.as_str(), sql_ts(now)],
        )?;
        Ok(User {
            id: UserId::from_stored(conn.last_insert_rowid()),
            role: Role::User,
            name: name.as_str().to_owned(),
            created_at: now,
        })
    }

    /// The user `workspace` belongs to, or `None` when no user is recorded for it: such a workspace
    /// is nobody's, so no user reaches it. Every create records its user before the workspace is
    /// listed, and the first start after the upgrade records the owner for the workspaces made
    /// before (see [`Store::adopt_unowned_workspaces`]).
    ///
    /// # Errors
    /// A database error.
    pub fn workspace_owner(&self, workspace: &WorkspaceId) -> Result<Option<UserId>, StoreError> {
        let id: Option<i64> = lock(&self.conn)
            .query_row(
                "SELECT user_id FROM workspace_owners WHERE workspace_id = ?1",
                [workspace.as_str()],
                |r| r.get(0),
            )
            .optional()?;
        Ok(id.map(UserId::from_stored))
    }

    /// Records the owner as the user of each of `workspaces` that has none, once: the workspaces
    /// listed when this build first starts were made before every create recorded its user, so
    /// they are the owner's. Later calls change nothing, so a workspace that lost its row is never
    /// handed to the owner. Returns how many it recorded.
    ///
    /// # Errors
    /// A database error; nothing is recorded then, and the next call tries again.
    pub fn adopt_unowned_workspaces<'a>(
        &self,
        _: &AllUsers,
        workspaces: impl IntoIterator<Item = &'a WorkspaceId>,
    ) -> Result<usize, StoreError> {
        let workspaces: Vec<&WorkspaceId> = workspaces.into_iter().collect();
        self.change(|conn, _now, _fx| {
            let open = conn.execute("DELETE FROM workspace_owners_adoption", [])? > 0;
            if !open {
                return Ok(0);
            }
            let mut stmt = conn.prepare(
                "INSERT OR IGNORE INTO workspace_owners (workspace_id, user_id) VALUES (?1, ?2)",
            )?;
            let mut recorded = 0;
            for workspace in workspaces {
                recorded += stmt.execute(params![workspace.as_str(), UserId::OWNER.get()])?;
            }
            Ok(recorded)
        })
    }

    /// Every recorded workspace user; a workspace that is not in the map is nobody's.
    ///
    /// # Errors
    /// A database error, or a stored id that is not a workspace id.
    pub fn workspace_owners(&self) -> Result<WorkspaceOwners, StoreError> {
        Ok(WorkspaceOwners(load_owners(&lock(&self.conn))?))
    }

    /// Records that `workspace` belongs to `user`, before the workspace exists anywhere else. A
    /// workspace's user never changes: recording the same user again is a no-op, another one is
    /// refused. From then on the rules decide its requests by `user`'s rules alone.
    ///
    /// # Errors
    /// [`StoreError::WorkspaceOwned`] when it already belongs to someone else,
    /// [`StoreError::UnknownUser`], or a database error.
    pub fn set_workspace_owner(
        &self,
        workspace: &WorkspaceId,
        user: UserId,
    ) -> Result<(), StoreError> {
        self.change(|conn, _now, _fx| {
            let known: bool = conn.query_row(
                "SELECT count(*) > 0 FROM users WHERE id = ?1",
                [user.get()],
                |r| r.get(0),
            )?;
            if !known {
                return Err(StoreError::UnknownUser(user));
            }
            let stored: Option<i64> = conn
                .query_row(
                    "SELECT user_id FROM workspace_owners WHERE workspace_id = ?1",
                    [workspace.as_str()],
                    |r| r.get(0),
                )
                .optional()?;
            match stored {
                Some(stored) if stored == user.get() => Ok(()),
                Some(_) => Err(StoreError::WorkspaceOwned(workspace.clone())),
                None => {
                    conn.execute(
                        "INSERT INTO workspace_owners (workspace_id, user_id) VALUES (?1, ?2)",
                        params![workspace.as_str(), user.get()],
                    )?;
                    Ok(())
                }
            }
        })
    }
}

/// The install's own reach across every user's data: the `*_every_user` reads and the one-shot
/// adoption of [`Store::adopt_unowned_workspaces`] take it, for the server's jobs (maintenance,
/// sweeps, metrics). [`Store::take_all_users`] hands out one per store, and the host takes it
/// before it serves anything; request handlers never get one, so every read they make is kept to
/// one user.
#[derive(Debug)]
pub struct AllUsers(());

impl AllUsers {
    pub(crate) fn new() -> Self {
        Self(())
    }
}

/// Who owns which workspace, read once for a list.
#[derive(Debug, Clone, Default)]
pub struct WorkspaceOwners(HashMap<WorkspaceId, UserId>);

impl WorkspaceOwners {
    /// Whether `workspace` is `user`'s. A workspace with no recorded user is nobody's.
    #[must_use]
    pub fn is(&self, workspace: &WorkspaceId, user: UserId) -> bool {
        self.0.get(workspace) == Some(&user)
    }
}

#[cfg(test)]
mod tests;
