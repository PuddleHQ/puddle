// SPDX-License-Identifier: AGPL-3.0-or-later
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
use crate::store::{insert_device, lock, revoke_devices_in, sql_ts, write_tx};
use crate::users::{Device, DeviceId, DeviceSession, UserId};

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

/// A password a check accepted: what [`AccountSecrets::add_device`] and
/// [`AccountSecrets::change_password`] make their step depend on, so a reset, a change or a
/// deletion that lands after the check wins. It holds the stored hash it was made from, never
/// shows it, and only this module reads it.
pub struct CheckedPassword {
    user: UserId,
    hash: String,
}

impl CheckedPassword {
    /// The user whose password this is.
    #[must_use]
    pub fn user(&self) -> UserId {
        self.user
    }
}

impl std::fmt::Debug for CheckedPassword {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CheckedPassword")
            .field("user", &self.user)
            .finish_non_exhaustive()
    }
}

/// Whether `user` still has the password `hash` and is not being deleted.
fn still_has(conn: &Connection, user: UserId, hash: &str) -> Result<bool, StoreError> {
    Ok(conn.query_row(
        "SELECT count(*) > 0 FROM account_passwords p JOIN users u ON u.id = p.user_id
          WHERE p.user_id = ?1 AND p.hash = ?2 AND u.deleting_at IS NULL",
        params![user.get(), hash],
        |r| r.get(0),
    )?)
}

/// Writes `hash` as `user`'s password, replacing the one before, for a user whose deletion has
/// not begun.
fn write_password(
    conn: &Connection,
    user: UserId,
    hash: &str,
    now: u64,
) -> Result<CheckedPassword, StoreError> {
    let written = conn.execute(
        "INSERT INTO account_passwords (user_id, hash, changed_at)
             SELECT id, ?2, ?3 FROM users WHERE id = ?1 AND deleting_at IS NULL
         ON CONFLICT (user_id) DO UPDATE SET hash = excluded.hash, changed_at = excluded.changed_at",
        params![user.get(), hash, sql_ts(now)],
    )?;
    if written == 0 {
        return Err(StoreError::UnknownUser(user));
    }
    Ok(CheckedPassword {
        user,
        hash: hash.to_owned(),
    })
}

/// Uses up the invite `digest` (see [`AccountSecrets::redeem_invite`]).
fn take_invite(conn: &Connection, digest: &[u8], now: u64) -> Result<Option<UserId>, StoreError> {
    let found: Option<(i64, i64, bool)> = conn
        .query_row(
            "DELETE FROM account_invites WHERE digest = ?1
             RETURNING user_id, expires_at,
                 (SELECT count(*) > 0 FROM users u
                  WHERE u.id = account_invites.user_id AND u.deleting_at IS NULL)",
            [digest],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let now = sql_ts(now);
    Ok(found
        .filter(|&(_, expires_at, active)| active && expires_at > now)
        .map(|(user, _, _)| UserId::from_stored(user)))
}

impl AccountSecrets {
    pub(crate) fn new(conn: Arc<Mutex<Connection>>, clock: Arc<dyn Clock>) -> Self {
        Self { conn, clock }
    }

    /// Stores `hash` (a self-describing password hash string) as `user`'s password, replacing the
    /// one before. A user whose deletion has begun gets none.
    ///
    /// # Errors
    /// [`StoreError::UnknownUser`] (also for a user being deleted), or a database error.
    pub fn set_password_hash(
        &self,
        user: UserId,
        hash: &str,
    ) -> Result<CheckedPassword, StoreError> {
        write_password(&lock(&self.conn), user, hash, self.clock.now_ms())
    }

    /// Replaces the password `checked` with `hash` and revokes every device of its user except
    /// `keep` (a change keeps the browser that made it), in one transaction: no failure leaves the
    /// new password with the old devices signed in. The ids revoked, or `None` (nothing written)
    /// when the password changed, went away or its user's deletion began since it was checked.
    ///
    /// # Errors
    /// A database error.
    pub fn change_password(
        &self,
        checked: &CheckedPassword,
        hash: &str,
        keep: Option<DeviceId>,
    ) -> Result<Option<Vec<DeviceId>>, StoreError> {
        let now = self.clock.now_ms();
        let mut conn = lock(&self.conn);
        let tx = write_tx(&mut conn)?;
        if !still_has(&tx, checked.user, &checked.hash)? {
            return Ok(None);
        }
        tx.execute(
            "UPDATE account_passwords SET hash = ?2, changed_at = ?3 WHERE user_id = ?1",
            params![checked.user.get(), hash, sql_ts(now)],
        )?;
        let revoked = revoke_devices_in(&tx, checked.user, keep, now)?;
        tx.commit()?;
        Ok(Some(revoked))
    }

    /// A new device for the user of `checked`, valid until `expires_at` (epoch ms), unless their
    /// password changed, went away or their deletion began since it was checked (then `None`).
    ///
    /// # Errors
    /// [`StoreError::DeviceName`], or a database error.
    pub fn add_device(
        &self,
        checked: &CheckedPassword,
        name: &str,
        expires_at: u64,
    ) -> Result<Option<Device>, StoreError> {
        let mut conn = lock(&self.conn);
        let tx = write_tx(&mut conn)?;
        if !still_has(&tx, checked.user, &checked.hash)? {
            return Ok(None);
        }
        let device = insert_device(&tx, checked.user, name, self.clock.now_ms(), expires_at)?;
        tx.commit()?;
        Ok(device)
    }

    /// Whether `user` has a password and `verify` accepts its stored hash. The hash goes to
    /// `verify` and nowhere else. A user being deleted has none.
    ///
    /// # Errors
    /// A database error.
    pub fn check_password(
        &self,
        user: UserId,
        verify: impl FnOnce(&str) -> bool,
    ) -> Result<bool, StoreError> {
        Ok(self.verify_password(user, verify)?.is_some())
    }

    /// [`check_password`](Self::check_password) that hands back what it accepted, for a step
    /// that must not happen if the password changes in the meantime. The check runs without the
    /// database held.
    ///
    /// # Errors
    /// A database error.
    pub fn verify_password(
        &self,
        user: UserId,
        verify: impl FnOnce(&str) -> bool,
    ) -> Result<Option<CheckedPassword>, StoreError> {
        let hash: Option<String> = lock(&self.conn)
            .query_row(
                "SELECT p.hash FROM account_passwords p JOIN users u ON u.id = p.user_id
                 WHERE p.user_id = ?1 AND u.deleting_at IS NULL",
                [user.get()],
                |r| r.get(0),
            )
            .optional()?;
        Ok(hash
            .filter(|hash| verify(hash))
            .map(|hash| CheckedPassword { user, hash }))
    }

    /// Whether `user` has a password. A user being deleted has none.
    ///
    /// # Errors
    /// A database error.
    pub fn has_password(&self, user: UserId) -> Result<bool, StoreError> {
        Ok(lock(&self.conn).query_row(
            "SELECT count(*) > 0 FROM account_passwords p JOIN users u ON u.id = p.user_id
             WHERE p.user_id = ?1 AND u.deleting_at IS NULL",
            [user.get()],
            |r| r.get(0),
        )?)
    }

    /// Whether the owner account has been made (by the first visitor).
    ///
    /// # Errors
    /// A database error.
    pub fn setup_done(&self) -> Result<bool, StoreError> {
        Ok(lock(&self.conn)
            .query_row("SELECT count(*) > 0 FROM account_setup", [], |r| r.get(0))?)
    }

    /// The first visitor's step: names the owner and gives it `hash` as its password, once.
    /// Whoever calls it first wins; every later call fails, however many ran at once.
    ///
    /// # Errors
    /// [`StoreError::SetupDone`] when the owner account is already made,
    /// [`StoreError::UserName`] when another user has the name, or a database error.
    pub fn create_owner(
        &self,
        name: &crate::UserName,
        hash: &str,
    ) -> Result<CheckedPassword, StoreError> {
        let mut conn = lock(&self.conn);
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let now = sql_ts(self.clock.now_ms());
        let inserted = tx.execute(
            "INSERT INTO account_setup (id, completed_at) VALUES (1, ?1) ON CONFLICT DO NOTHING",
            [now],
        )?;
        if inserted == 0 {
            return Err(StoreError::SetupDone);
        }
        let taken: bool = tx.query_row(
            "SELECT count(*) > 0 FROM users WHERE name = ?1 AND id <> ?2",
            params![name.as_str(), UserId::OWNER.get()],
            |r| r.get(0),
        )?;
        if taken {
            return Err(StoreError::UserName("that user name is taken".to_owned()));
        }
        tx.execute(
            "UPDATE users SET name = ?1 WHERE id = ?2",
            params![name.as_str(), UserId::OWNER.get()],
        )?;
        tx.execute(
            "INSERT INTO account_passwords (user_id, hash, changed_at) VALUES (?1, ?2, ?3)
             ON CONFLICT (user_id) DO UPDATE SET hash = excluded.hash, changed_at = excluded.changed_at",
            params![UserId::OWNER.get(), hash, now],
        )?;
        tx.commit()?;
        Ok(CheckedPassword {
            user: UserId::OWNER,
            hash: hash.to_owned(),
        })
    }

    /// Gives `device` the refresh token with this `digest` (a hash of the token, never the
    /// token), replacing the one before. `false`, writing nothing, when the device is unknown or
    /// revoked.
    ///
    /// # Errors
    /// [`StoreError::SessionDigestInvalid`] for a digest shorter than
    /// [`MIN_INVITE_DIGEST_BYTES`], or a database error.
    pub fn put_session(&self, device: DeviceId, digest: &[u8]) -> Result<bool, StoreError> {
        if digest.len() < MIN_INVITE_DIGEST_BYTES {
            return Err(StoreError::SessionDigestInvalid);
        }
        let written = lock(&self.conn).execute(
            "INSERT INTO account_sessions (device_id, refresh_digest)
             SELECT id, ?2 FROM devices WHERE id = ?1 AND revoked_at IS NULL
             ON CONFLICT (device_id) DO UPDATE SET refresh_digest = excluded.refresh_digest",
            params![device.get(), digest],
        )?;
        Ok(written > 0)
    }

    /// The device a refresh token's `digest` belongs to, while that device is neither revoked
    /// nor expired and its user is not being deleted.
    ///
    /// # Errors
    /// A database error, or a stored row that does not parse.
    pub fn session_for(&self, digest: &[u8]) -> Result<Option<DeviceSession>, StoreError> {
        let now = sql_ts(self.clock.now_ms());
        let found: Option<(i64, i64, String)> = lock(&self.conn)
            .query_row(
                "SELECT d.id, d.user_id, u.role FROM account_sessions s
                   JOIN devices d ON d.id = s.device_id JOIN users u ON u.id = d.user_id
                 WHERE s.refresh_digest = ?1 AND d.revoked_at IS NULL AND d.expires_at > ?2
                   AND u.deleting_at IS NULL",
                params![digest, now],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        found
            .map(|(device, user, role)| {
                Ok(DeviceSession {
                    device: DeviceId::from_stored(device),
                    user: UserId::from_stored(user),
                    role: crate::store::users::role_of(user, &role)?,
                })
            })
            .transpose()
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
    /// [`StoreError::UnknownUser`] (also for a user being deleted), [`StoreError::InviteInvalid`]
    /// for a digest shorter than [`MIN_INVITE_DIGEST_BYTES`], or a database error.
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
                 SELECT id, ?2, ?3 FROM users WHERE id = ?1 AND deleting_at IS NULL
             ON CONFLICT (user_id) DO UPDATE SET digest = excluded.digest, expires_at = excluded.expires_at",
            params![user.get(), digest, sql_ts(expires_at)],
        )?;
        if written == 0 {
            return Err(StoreError::UnknownUser(user));
        }
        Ok(())
    }

    /// Removes `user`'s open invite, if any (an account being deleted has no way in).
    ///
    /// # Errors
    /// A database error.
    pub fn clear_invite(&self, user: UserId) -> Result<(), StoreError> {
        lock(&self.conn).execute(
            "DELETE FROM account_invites WHERE user_id = ?1",
            [user.get()],
        )?;
        Ok(())
    }

    /// The user the invite `digest` is for, while it has not expired, without using it up.
    ///
    /// # Errors
    /// A database error.
    pub fn invite_holder(&self, digest: &[u8]) -> Result<Option<UserId>, StoreError> {
        let now = sql_ts(self.clock.now_ms());
        let found: Option<i64> = lock(&self.conn)
            .query_row(
                "SELECT user_id FROM account_invites WHERE digest = ?1 AND expires_at > ?2
                   AND user_id IN (SELECT id FROM users WHERE deleting_at IS NULL)",
                params![digest, now],
                |r| r.get(0),
            )
            .optional()?;
        Ok(found.map(UserId::from_stored))
    }

    /// Uses up the invite `digest`: the user it was for, if it exists, has not expired and its
    /// user is not being deleted. An invite works once; one that does not work is removed and
    /// answers `None`.
    ///
    /// # Errors
    /// A database error.
    pub fn redeem_invite(&self, digest: &[u8]) -> Result<Option<UserId>, StoreError> {
        take_invite(&lock(&self.conn), digest, self.clock.now_ms())
    }

    /// Uses up the invite `digest` and sets `hash` as its user's password, signing out every
    /// device of that user, in one transaction: an invite is never gone while its password is
    /// not stored. The password, and the ids of the devices revoked; `None` when the invite does
    /// not work (see [`Self::redeem_invite`]; one that does not work is still removed).
    ///
    /// # Errors
    /// A database error.
    pub fn redeem_invite_with_password(
        &self,
        digest: &[u8],
        hash: &str,
    ) -> Result<Option<(CheckedPassword, Vec<DeviceId>)>, StoreError> {
        let now = self.clock.now_ms();
        let mut conn = lock(&self.conn);
        let tx = write_tx(&mut conn)?;
        let Some(user) = take_invite(&tx, digest, now)? else {
            tx.commit()?;
            return Ok(None);
        };
        let checked = write_password(&tx, user, hash, now)?;
        let revoked = revoke_devices_in(&tx, user, None, now)?;
        tx.commit()?;
        Ok(Some((checked, revoked)))
    }
}

#[cfg(test)]
mod tests;
