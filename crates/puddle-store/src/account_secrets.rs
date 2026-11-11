// SPDX-License-Identifier: GPL-3.0-or-later
//! The sign-in secrets: each user's password hash and pending invite. They share the database
//! file with [`crate::Store`] but not its interface: no `Store` method reads these tables, and this
//! repository is handed out once ([`crate::Store::take_account_secrets`]) to the sign-in code, so the
//! code that serves the user list and the admin screen cannot reach a hash or an invite even by
//! mistake. Nothing here returns a secret: a hash is only ever given to a check the caller passes
//! in, and an invite is only ever looked up by its digest.

use std::sync::{Arc, Mutex};

use rusqlite::{Connection, OptionalExtension, params};

use crate::clock::Clock;
use crate::error::StoreError;
use crate::store::{lock, sql_ts};
use crate::users::UserId;

/// The shortest invite digest accepted, in bytes.
pub const MIN_INVITE_DIGEST_BYTES: usize = 16;

/// Reads and writes the sign-in secrets. See the module docs for who may hold it.
pub struct AccountSecrets {
    conn: Arc<Mutex<Connection>>,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for AccountSecrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AccountSecrets").finish_non_exhaustive()
    }
}

impl AccountSecrets {
    pub(crate) fn new(conn: Arc<Mutex<Connection>>, clock: Arc<dyn Clock>) -> Self {
        Self { conn, clock }
    }

    /// Stores `hash` (a self-describing password hash string) as `user`'s password, replacing the
    /// one before.
    ///
    /// # Errors
    /// [`StoreError::UnknownUser`], or a database error.
    pub fn set_password_hash(&self, user: UserId, hash: &str) -> Result<(), StoreError> {
        let conn = lock(&self.conn);
        let written = conn.execute(
            "INSERT INTO account_passwords (user_id, hash, changed_at)
                 SELECT id, ?2, ?3 FROM users WHERE id = ?1
             ON CONFLICT (user_id) DO UPDATE SET hash = excluded.hash, changed_at = excluded.changed_at",
            params![user.get(), hash, sql_ts(self.clock.now_ms())],
        )?;
        if written == 0 {
            return Err(StoreError::UnknownUser(user));
        }
        Ok(())
    }

    /// Whether `user` has a password and `verify` accepts its stored hash. The hash goes to
    /// `verify` and nowhere else.
    ///
    /// # Errors
    /// A database error.
    pub fn check_password(
        &self,
        user: UserId,
        verify: impl FnOnce(&str) -> bool,
    ) -> Result<bool, StoreError> {
        let hash: Option<String> = lock(&self.conn)
            .query_row(
                "SELECT hash FROM account_passwords WHERE user_id = ?1",
                [user.get()],
                |r| r.get(0),
            )
            .optional()?;
        Ok(hash.is_some_and(|hash| verify(&hash)))
    }

    /// Removes `user`'s password (a reset from the server), so they can't sign in until they set
    /// a new one through an invite.
    ///
    /// # Errors
    /// A database error.
    pub fn clear_password(&self, user: UserId) -> Result<(), StoreError> {
        lock(&self.conn).execute(
            "DELETE FROM account_passwords WHERE user_id = ?1",
            [user.get()],
        )?;
        Ok(())
    }

    /// Gives `user` one open invite, identified by `digest` (a hash of the link's secret, never
    /// the secret), valid until `expires_at` (epoch ms). It replaces any invite before.
    ///
    /// # Errors
    /// [`StoreError::UnknownUser`], [`StoreError::InviteInvalid`] for a digest shorter than
    /// [`MIN_INVITE_DIGEST_BYTES`], or a database error.
    pub fn put_invite(
        &self,
        user: UserId,
        digest: &[u8],
        expires_at: u64,
    ) -> Result<(), StoreError> {
        if digest.len() < MIN_INVITE_DIGEST_BYTES {
            return Err(StoreError::InviteInvalid);
        }
        let written = lock(&self.conn).execute(
            "INSERT INTO account_invites (user_id, digest, expires_at)
                 SELECT id, ?2, ?3 FROM users WHERE id = ?1
             ON CONFLICT (user_id) DO UPDATE SET digest = excluded.digest, expires_at = excluded.expires_at",
            params![user.get(), digest, sql_ts(expires_at)],
        )?;
        if written == 0 {
            return Err(StoreError::UnknownUser(user));
        }
        Ok(())
    }

    /// Uses up the invite `digest`: the user it was for, if it exists and has not expired. An
    /// invite works once; an expired one is removed and answers `None`.
    ///
    /// # Errors
    /// A database error.
    pub fn redeem_invite(&self, digest: &[u8]) -> Result<Option<UserId>, StoreError> {
        let now = sql_ts(self.clock.now_ms());
        let conn = lock(&self.conn);
        let found: Option<(i64, i64)> = conn
            .query_row(
                "DELETE FROM account_invites WHERE digest = ?1 RETURNING user_id, expires_at",
                [digest],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        Ok(found
            .filter(|&(_, expires_at)| expires_at > now)
            .map(|(user, _)| UserId::from_stored(user)))
    }
}

#[cfg(test)]
mod tests;
