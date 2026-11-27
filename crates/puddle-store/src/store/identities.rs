// SPDX-License-Identifier: AGPL-3.0-or-later
//! Identities and each workspace's Git settings in the store.
//! Credentials are stored as references to their sources; no value passes through.

use std::collections::HashSet;

use puddle_types::StoredId;
use puddle_types::{Event, WorkspaceId};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

use super::{Store, lock, sql_ts, users};
use crate::error::StoreError;
use crate::identity::{
    Author, CredentialBinding, Identity, IdentityDraft, IdentityId, Signing, check_attachable,
    collision,
};
use crate::users::UserId;
use crate::workspace_git::WorkspaceGit;

const COLUMNS: &str = "id, label, author_name, author_email, credentials, signing, is_default, \
                       created_at, changed_at";

/// Whether a user has no identity yet.
const NONE_YET: &str = "SELECT count(*) = 0 FROM identities WHERE user_id = ?1";

struct Raw {
    id: i64,
    label: String,
    name: String,
    email: String,
    credentials: String,
    signing: String,
    is_default: bool,
    created_at: i64,
    changed_at: i64,
}

fn raw(row: &rusqlite::Row<'_>) -> rusqlite::Result<Raw> {
    Ok(Raw {
        id: row.get(0)?,
        label: row.get(1)?,
        name: row.get(2)?,
        email: row.get(3)?,
        credentials: row.get(4)?,
        signing: row.get(5)?,
        is_default: row.get(6)?,
        created_at: row.get(7)?,
        changed_at: row.get(8)?,
    })
}

fn identity_of(raw: Raw) -> Result<Identity, StoreError> {
    let corrupt = |reason: String| StoreError::Corrupt {
        table: "identities",
        id: raw.id,
        reason,
    };
    let credentials: Vec<CredentialBinding> =
        serde_json::from_str(&raw.credentials).map_err(|e| corrupt(e.to_string()))?;
    let author = Author::new(&raw.name, &raw.email).map_err(|e| corrupt(e.to_string()))?;
    let signing = match raw.signing.as_str() {
        "none" => Signing::None,
        other => return Err(corrupt(format!("unknown signing {other}"))),
    };
    Ok(Identity {
        id: IdentityId(raw.id),
        label: raw.label,
        author,
        credentials,
        signing,
        is_default: raw.is_default,
        created_at: u64::try_from(raw.created_at).unwrap_or(0),
        changed_at: u64::try_from(raw.changed_at).unwrap_or(0),
    })
}

/// Every identity of `user`, in their order.
pub(super) fn all(conn: &Connection, user: UserId) -> Result<Vec<Identity>, StoreError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM identities WHERE user_id = ?1 ORDER BY position, id"
    ))?;
    let rows = stmt
        .query_map([user.get()], raw)?
        .collect::<Result<Vec<_>, _>>()?;
    rows.into_iter().map(identity_of).collect()
}

/// Every identity of every user: only for what the install keeps for all of them.
fn everyones(conn: &Connection) -> Result<Vec<Identity>, StoreError> {
    let mut stmt = conn.prepare(&format!("SELECT {COLUMNS} FROM identities ORDER BY id"))?;
    let rows = stmt.query_map([], raw)?.collect::<Result<Vec<_>, _>>()?;
    rows.into_iter().map(identity_of).collect()
}

/// One identity of `user`; another user's is unknown, as one that does not exist.
fn one(conn: &Connection, user: UserId, id: IdentityId) -> Result<Identity, StoreError> {
    let raw = conn
        .query_row(
            &format!("SELECT {COLUMNS} FROM identities WHERE id = ?1 AND user_id = ?2"),
            params![id.0, user.get()],
            raw,
        )
        .optional()?
        .ok_or(StoreError::UnknownIdentity(id))?;
    identity_of(raw)
}

fn attached_ids(conn: &Connection, ws: &WorkspaceId) -> Result<Vec<IdentityId>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT identity_id FROM workspace_identities WHERE workspace_id = ?1 ORDER BY position",
    )?;
    let ids = stmt
        .query_map([ws.as_str()], |r| r.get::<_, i64>(0).map(IdentityId))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ids)
}

pub(super) fn attached(conn: &Connection, ws: &WorkspaceId) -> Result<Vec<Identity>, StoreError> {
    let user = users::owner_in(conn, ws)?;
    attached_ids(conn, ws)?
        .into_iter()
        .map(|id| one(conn, user, id))
        .collect()
}

fn workspaces_using(conn: &Connection, id: IdentityId) -> Result<Vec<WorkspaceId>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT workspace_id FROM workspace_identities WHERE identity_id = ?1 ORDER BY 1",
    )?;
    let names = stmt
        .query_map([id.0], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(names
        .into_iter()
        .filter_map(|n| WorkspaceId::new(&n).ok())
        .collect())
}

pub(super) fn write_list(
    tx: &Transaction<'_>,
    ws: &WorkspaceId,
    ids: &[IdentityId],
) -> Result<(), StoreError> {
    tx.execute(
        "DELETE FROM workspace_identities WHERE workspace_id = ?1",
        [ws.as_str()],
    )?;
    for (position, id) in ids.iter().enumerate() {
        tx.execute(
            "INSERT INTO workspace_identities (workspace_id, identity_id, position) VALUES (?1, ?2, ?3)",
            params![ws.as_str(), id.0, i64::try_from(position).unwrap_or(i64::MAX)],
        )?;
    }
    Ok(())
}

/// Refuses a list of identities in which two cover the same place.
pub(super) fn check_list(list: &[Identity]) -> Result<(), StoreError> {
    for (i, a) in list.iter().enumerate() {
        if let Some(c) = list.iter().skip(i + 1).find_map(|b| collision(a, b)) {
            return Err(StoreError::IdentityCollision(c));
        }
    }
    Ok(())
}

fn label_free(
    tx: &Transaction<'_>,
    user: UserId,
    label: &str,
    except: Option<IdentityId>,
) -> Result<(), StoreError> {
    let taken: Option<i64> = tx
        .query_row(
            "SELECT id FROM identities WHERE label = ?1 AND id IS NOT ?2 AND user_id = ?3",
            params![label, except.map(|i| i.0), user.get()],
            |r| r.get(0),
        )
        .optional()?;
    if taken.is_some() {
        return Err(StoreError::IdentityLabelTaken(label.to_owned()));
    }
    Ok(())
}

fn credentials_json(credentials: &[CredentialBinding]) -> Result<String, StoreError> {
    serde_json::to_string(credentials)
        .map_err(|e| StoreError::IdentityInvalid(format!("credentials can't be stored: {e}")))
}

pub(super) fn changed_for(fx: &mut Vec<Event>, workspaces: Vec<WorkspaceId>) {
    fx.push(Event::IdentitiesChanged {});
    fx.extend(
        workspaces
            .into_iter()
            .map(|workspace| Event::WorkspaceGitChanged { workspace }),
    );
}

/// Replaces `ws`'s identity list with `ids` inside the caller's transaction (see
/// [`Store::set_workspace_identities`] for the errors).
fn set_list_in(
    tx: &Transaction<'_>,
    fx: &mut Vec<Event>,
    ws: &WorkspaceId,
    ids: &[IdentityId],
) -> Result<(), StoreError> {
    let user = users::owner_in(tx, ws)?;
    let mut list = Vec::with_capacity(ids.len());
    for id in ids {
        if list.iter().any(|i: &Identity| i.id == *id) {
            return Err(StoreError::IdentityInvalid(
                "an identity is listed twice".into(),
            ));
        }
        list.push(one(tx, user, *id)?);
    }
    check_list(&list)?;
    write_list(tx, ws, ids)?;
    fx.push(Event::WorkspaceGitChanged {
        workspace: ws.clone(),
    });
    Ok(())
}

impl Store {
    /// Every id a value in the credential store may still be kept under: the pasted tokens the
    /// identities of every user read and the values of environment secrets. A value under any
    /// other id has nothing that names it.
    ///
    /// # Errors
    /// A database error, or a stored row that doesn't parse: a caller that removes what is not
    /// listed must not go on then.
    pub fn secret_ids_in_use(&self) -> Result<HashSet<StoredId>, StoreError> {
        let conn = lock(&self.conn);
        let mut ids: HashSet<StoredId> = everyones(&conn)?
            .iter()
            .flat_map(|identity| &identity.credentials)
            .filter_map(|binding| binding.stored_id().cloned())
            .collect();
        let mut stmt =
            conn.prepare("SELECT id, secret_id FROM env_vars WHERE secret_id IS NOT NULL")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (row_id, secret_id) = row?;
            ids.insert(StoredId::new(secret_id).map_err(|e| StoreError::Corrupt {
                table: "env_vars",
                id: row_id,
                reason: e.to_string(),
            })?);
        }
        Ok(ids)
    }

    /// Every identity of `user`, in the order they keep them.
    ///
    /// # Errors
    /// A database error, or a stored row that doesn't parse.
    pub fn identities(&self, user: UserId) -> Result<Vec<Identity>, StoreError> {
        all(&lock(&self.conn), user)
    }

    /// One identity of `user`.
    ///
    /// # Errors
    /// [`StoreError::UnknownIdentity`] (also for another user's), or a database error.
    pub fn identity(&self, user: UserId, id: IdentityId) -> Result<Identity, StoreError> {
        one(&lock(&self.conn), user, id)
    }

    /// The default identity of `user`: the one a new workspace of theirs gets when none covers
    /// its URL.
    ///
    /// # Errors
    /// A database error.
    pub fn default_identity(&self, user: UserId) -> Result<Option<Identity>, StoreError> {
        Ok(all(&lock(&self.conn), user)?
            .into_iter()
            .find(|i| i.is_default))
    }

    /// The workspaces an identity is on.
    ///
    /// # Errors
    /// A database error.
    pub fn identity_workspaces(&self, id: IdentityId) -> Result<Vec<WorkspaceId>, StoreError> {
        workspaces_using(&lock(&self.conn), id)
    }

    /// Makes an identity of `user`, last in their order; their first one is their default.
    ///
    /// # Errors
    /// [`StoreError::IdentityInvalid`], [`StoreError::IdentityLabelTaken`], or a database error.
    pub fn create_identity(
        &self,
        user: UserId,
        draft: IdentityDraft,
    ) -> Result<Identity, StoreError> {
        let draft = draft.checked()?;
        let id = self.change(|tx, now, fx| {
            label_free(tx, user, &draft.label, None)?;
            let first: bool = tx.query_row(NONE_YET, [user.get()], |r| r.get(0))?;
            tx.execute(
                "INSERT INTO identities (user_id, label, author_name, author_email, credentials,
                                         position, is_default, created_at, changed_at)
                 VALUES (?7, ?1, ?2, ?3, ?4,
                         (SELECT coalesce(max(position) + 1, 0) FROM identities
                          WHERE user_id = ?7),
                         ?5, ?6, ?6)",
                params![
                    draft.label,
                    draft.author.name,
                    draft.author.email,
                    credentials_json(&draft.credentials)?,
                    first,
                    sql_ts(now),
                    user.get(),
                ],
            )?;
            changed_for(fx, Vec::new());
            Ok(IdentityId(tx.last_insert_rowid()))
        })?;
        tracing::info!(identity = %id, "identity created");
        self.identity(user, id)
    }

    /// Replaces an identity's label, author and credentials. Refused with a collision when the new
    /// coverage meets another identity on a workspace that has both.
    ///
    /// # Errors
    /// [`StoreError::UnknownIdentity`], [`StoreError::IdentityInvalid`],
    /// [`StoreError::IdentityLabelTaken`], [`StoreError::IdentityCollision`], or a database error.
    pub fn update_identity(
        &self,
        user: UserId,
        id: IdentityId,
        draft: IdentityDraft,
    ) -> Result<Identity, StoreError> {
        let draft = draft.checked()?;
        self.change(|tx, now, fx| {
            let before = one(tx, user, id)?;
            label_free(tx, user, &draft.label, Some(id))?;
            let mut after = before;
            after.label.clone_from(&draft.label);
            after.author = draft.author.clone();
            after.credentials.clone_from(&draft.credentials);
            let workspaces = workspaces_using(tx, id)?;
            for ws in &workspaces {
                check_attachable(&attached(tx, ws)?, &after)
                    .map_err(StoreError::IdentityCollision)?;
            }
            tx.execute(
                "UPDATE identities SET label = ?2, author_name = ?3, author_email = ?4,
                        credentials = ?5, changed_at = ?6 WHERE id = ?1",
                params![
                    id.0,
                    draft.label,
                    draft.author.name,
                    draft.author.email,
                    credentials_json(&draft.credentials)?,
                    sql_ts(now),
                ],
            )?;
            changed_for(fx, workspaces);
            Ok(())
        })?;
        self.identity(user, id)
    }

    /// Deletes an identity of `user` and takes it off every workspace; returns the workspaces it
    /// left. The default passes to the first of their identities in their order.
    ///
    /// # Errors
    /// [`StoreError::UnknownIdentity`], or a database error.
    pub fn delete_identity(
        &self,
        user: UserId,
        id: IdentityId,
    ) -> Result<Vec<WorkspaceId>, StoreError> {
        let left = self.change(|tx, _now, fx| {
            let gone = one(tx, user, id)?;
            let workspaces = workspaces_using(tx, id)?;
            tx.execute(
                "DELETE FROM workspace_identities WHERE identity_id = ?1",
                [id.0],
            )?;
            tx.execute("DELETE FROM identities WHERE id = ?1", [id.0])?;
            if gone.is_default {
                tx.execute(
                    "UPDATE identities SET is_default = 1
                     WHERE id = (SELECT id FROM identities WHERE user_id = ?1
                                 ORDER BY position, id LIMIT 1)",
                    [user.get()],
                )?;
            }
            for ws in &workspaces {
                let rest: Vec<IdentityId> = attached_ids(tx, ws)?;
                write_list(tx, ws, &rest)?;
            }
            changed_for(fx, workspaces.clone());
            Ok(workspaces)
        })?;
        tracing::info!(identity = %id, workspaces = left.len(), "identity deleted");
        Ok(left)
    }

    /// Makes `id` the default identity of `user`.
    ///
    /// # Errors
    /// [`StoreError::UnknownIdentity`], or a database error.
    pub fn set_default_identity(
        &self,
        user: UserId,
        id: IdentityId,
    ) -> Result<Identity, StoreError> {
        self.change(|tx, _now, fx| {
            one(tx, user, id)?;
            tx.execute(
                "UPDATE identities SET is_default = 0 WHERE is_default = 1 AND user_id = ?1",
                [user.get()],
            )?;
            tx.execute("UPDATE identities SET is_default = 1 WHERE id = ?1", [id.0])?;
            changed_for(fx, Vec::new());
            Ok(())
        })?;
        self.identity(user, id)
    }

    /// Puts the identities of `user` in a new order; `ids` must name every one of them once.
    ///
    /// # Errors
    /// [`StoreError::IdentityInvalid`] when `ids` is not a permutation of their identities, or a
    /// database error.
    pub fn reorder_identities(
        &self,
        user: UserId,
        ids: &[IdentityId],
    ) -> Result<Vec<Identity>, StoreError> {
        self.change(|tx, _now, fx| {
            let mut have: Vec<IdentityId> = all(tx, user)?.into_iter().map(|i| i.id).collect();
            let mut want = ids.to_vec();
            have.sort();
            want.sort();
            if have != want {
                return Err(StoreError::IdentityInvalid(
                    "the order must list every identity exactly once".into(),
                ));
            }
            for (position, id) in ids.iter().enumerate() {
                tx.execute(
                    "UPDATE identities SET position = ?2 WHERE id = ?1",
                    params![id.0, i64::try_from(position).unwrap_or(i64::MAX)],
                )?;
            }
            changed_for(fx, Vec::new());
            Ok(())
        })?;
        self.identities(user)
    }

    /// Replaces the workspace's ordered identity list, from its user's identities.
    ///
    /// # Errors
    /// [`StoreError::UnknownIdentity`] (also another user's), [`StoreError::IdentityInvalid`] for a
    /// repeated id,
    /// [`StoreError::IdentityCollision`] naming both when two cover the same place, or a
    /// database error. Nothing changes on an error.
    pub fn set_workspace_identities(
        &self,
        ws: &WorkspaceId,
        ids: &[IdentityId],
    ) -> Result<WorkspaceGit, StoreError> {
        self.change(|tx, _now, fx| set_list_in(tx, fx, ws, ids))?;
        self.workspace_git(ws)
    }

    /// Adds an identity to the workspace, last unless `position` says otherwise.
    ///
    /// # Errors
    /// As [`Store::set_workspace_identities`], and [`StoreError::IdentityAttached`] when it is
    /// already there.
    pub fn attach_identity(
        &self,
        ws: &WorkspaceId,
        id: IdentityId,
        position: Option<usize>,
    ) -> Result<WorkspaceGit, StoreError> {
        self.change(|tx, _now, fx| {
            let candidate = one(tx, users::owner_in(tx, ws)?, id)?;
            let mut ids = attached_ids(tx, ws)?;
            if ids.contains(&id) {
                return Err(StoreError::IdentityAttached {
                    label: candidate.label,
                });
            }
            ids.insert(position.unwrap_or(ids.len()).min(ids.len()), id);
            set_list_in(tx, fx, ws, &ids)
        })?;
        self.workspace_git(ws)
    }

    /// Takes an identity off the workspace.
    ///
    /// # Errors
    /// [`StoreError::UnknownIdentity`] when it is not on the workspace, or a database error.
    pub fn detach_identity(
        &self,
        ws: &WorkspaceId,
        id: IdentityId,
    ) -> Result<WorkspaceGit, StoreError> {
        self.change(|tx, _now, fx| {
            let mut ids = attached_ids(tx, ws)?;
            let before = ids.len();
            ids.retain(|i| *i != id);
            if ids.len() == before {
                return Err(StoreError::UnknownIdentity(id));
            }
            set_list_in(tx, fx, ws, &ids)
        })?;
        self.workspace_git(ws)
    }
}

#[cfg(test)]
mod tests;
