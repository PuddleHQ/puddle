// SPDX-License-Identifier: GPL-3.0-or-later
//! Users and who owns which workspace. Credentials are not here: see [`crate::AccountSecrets`].

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use puddle_types::WorkspaceId;
use rusqlite::{OptionalExtension, Row, params};

use super::{Store, corrupt, lock, sql_ts, stored_ts};
use crate::account_secrets::AccountSecrets;
use crate::error::StoreError;
use crate::users::{Role, User, UserId, UserName};

fn user_of(row: &Row<'_>) -> Result<User, StoreError> {
    let id: i64 = row.get(0)?;
    let role: String = row.get(1)?;
    Ok(User {
        id: UserId::from_stored(id),
        role: Role::parse(&role).ok_or_else(|| corrupt("users", id, "unknown role"))?,
        name: row.get(2)?,
        created_at: stored_ts("users", id, row.get(3)?)?,
    })
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

    /// The user `workspace` belongs to: the one recorded for it, else the owner (every workspace
    /// made before there were users, and every one the owner makes).
    ///
    /// # Errors
    /// A database error.
    pub fn workspace_owner(&self, workspace: &WorkspaceId) -> Result<UserId, StoreError> {
        let conn = lock(&self.conn);
        let id: Option<i64> = conn
            .query_row(
                "SELECT user_id FROM workspace_owners WHERE workspace_id = ?1",
                [workspace.as_str()],
                |r| r.get(0),
            )
            .optional()?;
        Ok(id.map_or(UserId::OWNER, UserId::from_stored))
    }

    /// Every recorded workspace user; a workspace that is not in the map is the owner's.
    ///
    /// # Errors
    /// A database error, or a stored id that is not a workspace id.
    pub fn workspace_owners(&self) -> Result<WorkspaceOwners, StoreError> {
        let conn = lock(&self.conn);
        let mut stmt = conn.prepare("SELECT workspace_id, user_id FROM workspace_owners")?;
        let mut rows = stmt.query([])?;
        let mut map = HashMap::new();
        while let Some(row) = rows.next()? {
            let id: String = row.get(0)?;
            let workspace =
                WorkspaceId::new(&id).map_err(|e| corrupt("workspace_owners", 0, e.to_string()))?;
            map.insert(workspace, UserId::from_stored(row.get(1)?));
        }
        Ok(WorkspaceOwners(map))
    }

    /// Records that `workspace` belongs to `user`, before the workspace exists anywhere else. A
    /// workspace's user never changes: recording the same user again is a no-op, another one is
    /// refused.
    ///
    /// # Errors
    /// [`StoreError::WorkspaceOwned`] when it already belongs to someone else,
    /// [`StoreError::UnknownUser`], or a database error.
    pub fn set_workspace_owner(
        &self,
        workspace: &WorkspaceId,
        user: UserId,
    ) -> Result<(), StoreError> {
        let conn = lock(&self.conn);
        let known: bool = conn.query_row(
            "SELECT count(*) > 0 FROM users WHERE id = ?1",
            [user.get()],
            |r| r.get(0),
        )?;
        if !known {
            return Err(StoreError::UnknownUser(user));
        }
        conn.execute(
            "INSERT INTO workspace_owners (workspace_id, user_id) VALUES (?1, ?2)
             ON CONFLICT (workspace_id) DO NOTHING",
            params![workspace.as_str(), user.get()],
        )?;
        let stored: i64 = conn.query_row(
            "SELECT user_id FROM workspace_owners WHERE workspace_id = ?1",
            [workspace.as_str()],
            |r| r.get(0),
        )?;
        if stored == user.get() {
            Ok(())
        } else {
            Err(StoreError::WorkspaceOwned(workspace.clone()))
        }
    }
}

/// Who owns which workspace, read once for a list: [`WorkspaceOwners::of`] answers the owner for
/// a workspace that has no row.
#[derive(Debug, Clone, Default)]
pub struct WorkspaceOwners(HashMap<WorkspaceId, UserId>);

impl WorkspaceOwners {
    /// The user `workspace` belongs to.
    #[must_use]
    pub fn of(&self, workspace: &WorkspaceId) -> UserId {
        self.0.get(workspace).copied().unwrap_or(UserId::OWNER)
    }
}

#[cfg(test)]
mod tests;
