// SPDX-License-Identifier: AGPL-3.0-or-later
//! The admin side of users: the list with its counts, adding and deleting a user, and the audit of
//! both. Credentials are not here (see [`crate::AccountSecrets`]); what a user keeps stays theirs:
//! these methods count and remove it and never read it back.

use puddle_types::{Event, WorkspaceId};
use rusqlite::{OptionalExtension, Transaction, params};

use super::{Store, corrupt, lock, sql_ts, stored_ts, write_tx};
use crate::admin::{AdminAction, AdminAuditEntry, MAX_ADMIN_AUDIT_LINES, UserFacts};
use crate::error::StoreError;
use crate::users::{Role, User, UserId, UserName};

fn name_of(tx: &Transaction<'_>, user: UserId) -> Result<String, StoreError> {
    tx.query_row("SELECT name FROM users WHERE id = ?1", [user.get()], |r| {
        r.get(0)
    })
    .optional()?
    .ok_or(StoreError::UnknownUser(user))
}

fn note(
    tx: &Transaction<'_>,
    now: u64,
    actor: UserId,
    action: AdminAction,
    target: UserId,
    target_name: &str,
) -> Result<(), StoreError> {
    let actor_name = name_of(tx, actor)?;
    tx.execute(
        "INSERT INTO admin_audit (at, actor_user_id, actor_name, action, target_user_id, target_name)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            sql_ts(now),
            actor.get(),
            actor_name,
            action.as_str(),
            target.get(),
            target_name
        ],
    )?;
    Ok(())
}

fn load_user(tx: &Transaction<'_>, id: UserId) -> Result<User, StoreError> {
    let (role, name, created): (String, String, i64) = tx
        .query_row(
            "SELECT role, name, created_at FROM users WHERE id = ?1",
            [id.get()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?
        .ok_or(StoreError::UnknownUser(id))?;
    Ok(User {
        id,
        role: super::users::role_of(id.get(), &role)?,
        name,
        created_at: stored_ts("users", id.get(), created)?,
    })
}

impl Store {
    /// Adds a plain user on behalf of `actor` and writes the admin audit line, in one step. The
    /// user starts with no password: an invite gives them one.
    ///
    /// # Errors
    /// [`StoreError::UserName`] when the name is taken (ignoring ASCII case), or a database error.
    pub fn admin_add_user(&self, actor: UserId, name: &UserName) -> Result<User, StoreError> {
        let mut conn = lock(&self.conn);
        let now = self.clock.now_ms();
        let tx = write_tx(&mut conn)?;
        let taken: bool = tx.query_row(
            "SELECT count(*) > 0 FROM users WHERE name = ?1",
            [name.as_str()],
            |r| r.get(0),
        )?;
        if taken {
            return Err(StoreError::UserName("that user name is taken".to_owned()));
        }
        tx.execute(
            "INSERT INTO users (role, name, created_at) VALUES ('user', ?1, ?2)",
            params![name.as_str(), sql_ts(now)],
        )?;
        let id = UserId::from_stored(tx.last_insert_rowid());
        note(&tx, now, actor, AdminAction::UserCreated, id, name.as_str())?;
        tx.commit()?;
        Ok(User {
            id,
            role: Role::User,
            name: name.as_str().to_owned(),
            created_at: now,
        })
    }

    /// Writes the admin audit line for a new invite link for `target`.
    ///
    /// # Errors
    /// [`StoreError::UnknownUser`], or a database error.
    pub fn note_invite_reissued(&self, actor: UserId, target: UserId) -> Result<(), StoreError> {
        let mut conn = lock(&self.conn);
        let tx = write_tx(&mut conn)?;
        let name = name_of(&tx, target)?;
        note(
            &tx,
            self.clock.now_ms(),
            actor,
            AdminAction::InviteReissued,
            target,
            &name,
        )?;
        Ok(tx.commit()?)
    }

    /// Every user with the counts the admin list shows, the owner first, then by id.
    ///
    /// # Errors
    /// A database error, or a stored row that does not parse.
    pub fn user_facts(&self) -> Result<Vec<UserFacts>, StoreError> {
        let conn = lock(&self.conn);
        let now = sql_ts(self.clock.now_ms());
        let mut stmt = conn.prepare(
            "SELECT u.id, u.role, u.name, u.created_at, u.deleting_at,
                    (SELECT count(*) FROM devices d
                      WHERE d.user_id = u.id AND d.revoked_at IS NULL AND d.expires_at > ?1),
                    (SELECT max(d.created_at) FROM devices d WHERE d.user_id = u.id)
             FROM users u ORDER BY u.role = 'owner' DESC, u.id",
        )?;
        let mut rows = stmt.query([now])?;
        let mut facts = Vec::new();
        while let Some(row) = rows.next()? {
            let id: i64 = row.get(0)?;
            let role: String = row.get(1)?;
            let deleting: Option<i64> = row.get(4)?;
            let devices: i64 = row.get(5)?;
            let last: Option<i64> = row.get(6)?;
            facts.push(UserFacts {
                user: User {
                    id: UserId::from_stored(id),
                    role: super::users::role_of(id, &role)?,
                    name: row.get(2)?,
                    created_at: stored_ts("users", id, row.get(3)?)?,
                },
                deleting_since: deleting.map(|t| stored_ts("users", id, t)).transpose()?,
                devices: u32::try_from(devices).map_err(|_| corrupt("devices", id, "count"))?,
                last_sign_in: last.map(|t| stored_ts("devices", id, t)).transpose()?,
            });
        }
        Ok(facts)
    }

    /// The workspaces recorded for `user`, including any whose files are already gone (the
    /// record outlives them). The caller matches them against the workspaces that exist.
    ///
    /// # Errors
    /// A database error, or a stored id that is not a workspace id.
    pub fn workspaces_of(&self, user: UserId) -> Result<Vec<WorkspaceId>, StoreError> {
        let conn = lock(&self.conn);
        let mut stmt = conn
            .prepare("SELECT workspace_id FROM workspace_owners WHERE user_id = ?1 ORDER BY 1")?;
        let ids = stmt
            .query_map([user.get()], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        ids.into_iter()
            .map(|id| {
                WorkspaceId::new(&id).map_err(|e| corrupt("workspace_owners", 0, e.to_string()))
            })
            .collect()
    }

    /// Starts deleting `target` on behalf of `actor`: the user stays listed as deleting, and no
    /// code path signs them in again. Writes the audit line the first time; calling it again for
    /// a user already being deleted changes nothing. The owner is never deleted.
    ///
    /// # Errors
    /// [`StoreError::OwnerKept`] for the owner, [`StoreError::UnknownUser`], or a database error.
    pub fn begin_user_delete(&self, actor: UserId, target: UserId) -> Result<User, StoreError> {
        let mut conn = lock(&self.conn);
        let now = self.clock.now_ms();
        let tx = write_tx(&mut conn)?;
        let user = load_user(&tx, target)?;
        if user.role == Role::Owner {
            return Err(StoreError::OwnerKept);
        }
        let started = tx.execute(
            "UPDATE users SET deleting_at = ?2 WHERE id = ?1 AND deleting_at IS NULL",
            params![target.get(), sql_ts(now)],
        )?;
        if started > 0 {
            note(
                &tx,
                now,
                actor,
                AdminAction::UserDeleteStarted,
                target,
                &user.name,
            )?;
        }
        tx.commit()?;
        Ok(user)
    }

    /// The users whose deletion has begun and is not finished.
    ///
    /// # Errors
    /// A database error, or a stored row that does not parse.
    pub fn users_deleting(&self) -> Result<Vec<User>, StoreError> {
        Ok(self
            .user_facts()?
            .into_iter()
            .filter(|f| f.deleting_since.is_some())
            .map(|f| f.user)
            .collect())
    }

    /// Finishes deleting a user whose deletion began: removes their devices and sessions, rules,
    /// rule sets, environment, identities, Git defaults, audit records, the records of which
    /// workspaces were theirs, and the account. Writes the audit line (who started it, whom, when
    /// it ended). The caller has deleted their workspaces first; values in the operating system's
    /// credential store that nothing names any more are swept by the credential sweep.
    ///
    /// # Errors
    /// [`StoreError::UnknownUser`] for an id that is gone or not being deleted, or a database
    /// error (nothing is removed then).
    pub fn finish_user_delete(&self, target: UserId) -> Result<(), StoreError> {
        self.change(|tx, now, fx| {
            let name = name_of(tx, target)?;
            let begun: Option<Option<i64>> = tx
                .query_row(
                    "SELECT deleting_at FROM users WHERE id = ?1 AND role <> 'owner'",
                    [target.get()],
                    |r| r.get(0),
                )
                .optional()?;
            if begun.flatten().is_none() {
                return Err(StoreError::UnknownUser(target));
            }
            let actor: i64 = tx
                .query_row(
                    "SELECT actor_user_id FROM admin_audit
                     WHERE target_user_id = ?1 AND action = 'user_delete_started'
                     ORDER BY id DESC LIMIT 1",
                    [target.get()],
                    |r| r.get(0),
                )
                .optional()?
                .unwrap_or(UserId::OWNER.get());
            let user = [target.get()];
            for sql in [
                "DELETE FROM env_stand_ins WHERE env_id IN (SELECT id FROM env_vars WHERE user_id = ?1)",
                "DELETE FROM env_vars WHERE user_id = ?1",
                "DELETE FROM rules WHERE user_id = ?1",
                "DELETE FROM request_rules WHERE user_id = ?1",
                "DELETE FROM rule_set_switches WHERE user_id = ?1",
                "DELETE FROM rule_sets WHERE user_id = ?1",
                "DELETE FROM audit WHERE user_id = ?1",
                "DELETE FROM identities WHERE user_id = ?1",
                "DELETE FROM git_defaults WHERE user_id = ?1",
                "DELETE FROM workspace_owners WHERE user_id = ?1",
                "DELETE FROM users WHERE id = ?1",
            ] {
                tx.execute(sql, user)?;
            }
            // The owner never comes here: the query above refused it, and a trigger would too.
            note(
                tx,
                now,
                UserId::from_stored(actor),
                AdminAction::UserDeleted,
                target,
                &name,
            )?;
            fx.push(Event::RulesChanged {});
            Ok(())
        })
    }

    /// The newest admin audit lines, newest first, at most `limit` (and
    /// [`MAX_ADMIN_AUDIT_LINES`]).
    ///
    /// # Errors
    /// A database error, or a stored row that does not parse.
    pub fn admin_audit(&self, limit: u32) -> Result<Vec<AdminAuditEntry>, StoreError> {
        let conn = lock(&self.conn);
        let mut stmt = conn.prepare(
            "SELECT id, at, actor_user_id, actor_name, action, target_user_id, target_name
             FROM admin_audit ORDER BY id DESC LIMIT ?1",
        )?;
        let mut rows = stmt.query([limit.min(MAX_ADMIN_AUDIT_LINES)])?;
        let mut lines = Vec::new();
        while let Some(row) = rows.next()? {
            let id: i64 = row.get(0)?;
            let action: String = row.get(4)?;
            lines.push(AdminAuditEntry {
                id,
                at: stored_ts("admin_audit", id, row.get(1)?)?,
                actor: UserId::from_stored(row.get(2)?),
                actor_name: row.get(3)?,
                action: AdminAction::parse(&action)
                    .ok_or_else(|| corrupt("admin_audit", id, "unknown action"))?,
                target: UserId::from_stored(row.get(5)?),
                target_name: row.get(6)?,
            });
        }
        Ok(lines)
    }
}

#[cfg(test)]
mod tests;
