// SPDX-License-Identifier: AGPL-3.0-or-later
//! Devices (signed-in browsers) and the sign-in methods. A device row carries no credential: the
//! digest of its refresh token lives behind [`crate::AccountSecrets`].

use rusqlite::{OptionalExtension, Row, params};

use super::{Store, corrupt, lock, sql_ts, stored_ts};
use crate::error::StoreError;
use crate::users::{Device, DeviceId, DeviceSession, SignInMethod, UserId, UserName, device_name};

fn device_of(row: &Row<'_>) -> Result<Device, StoreError> {
    let id: i64 = row.get(0)?;
    Ok(Device {
        id: DeviceId::from_stored(id),
        name: row.get(1)?,
        created_at: stored_ts("devices", id, row.get(2)?)?,
        last_seen_at: stored_ts("devices", id, row.get(3)?)?,
        expires_at: stored_ts("devices", id, row.get(4)?)?,
    })
}

impl Store {
    /// The user called `name` (ignoring ASCII case), if there is one.
    ///
    /// # Errors
    /// A database error, or a stored row that does not parse.
    pub fn user_named(&self, name: &str) -> Result<Option<crate::User>, StoreError> {
        // A name that could not be one never matches, and never reaches a query.
        if UserName::new(name).is_err() {
            return Ok(None);
        }
        let found: Option<i64> = lock(&self.conn)
            .query_row("SELECT id FROM users WHERE name = ?1", [name], |r| r.get(0))
            .optional()?;
        found
            .map(|id| self.user(UserId::from_stored(id)))
            .transpose()
    }

    /// A new device for `user`, valid until `expires_at` (epoch ms).
    ///
    /// # Errors
    /// [`StoreError::DeviceName`], [`StoreError::UnknownUser`], or a database error.
    pub fn add_device(
        &self,
        user: UserId,
        name: &str,
        expires_at: u64,
    ) -> Result<Device, StoreError> {
        let name = device_name(name)?;
        let conn = lock(&self.conn);
        let now = self.clock.now_ms();
        let written = conn.execute(
            "INSERT INTO devices (user_id, name, created_at, last_seen_at, expires_at)
                 SELECT id, ?2, ?3, ?3, ?4 FROM users WHERE id = ?1",
            params![user.get(), name, sql_ts(now), sql_ts(expires_at)],
        )?;
        if written == 0 {
            return Err(StoreError::UnknownUser(user));
        }
        Ok(Device {
            id: DeviceId::from_stored(conn.last_insert_rowid()),
            name,
            created_at: now,
            last_seen_at: now,
            expires_at,
        })
    }

    /// `user`'s devices that still work (not revoked, not expired), the one used last first.
    ///
    /// # Errors
    /// A database error, or a stored row that does not parse.
    pub fn devices(&self, user: UserId) -> Result<Vec<Device>, StoreError> {
        let now = sql_ts(self.clock.now_ms());
        let conn = lock(&self.conn);
        let mut stmt = conn.prepare(
            "SELECT id, name, created_at, last_seen_at, expires_at FROM devices
             WHERE user_id = ?1 AND revoked_at IS NULL AND expires_at > ?2
             ORDER BY last_seen_at DESC, id DESC",
        )?;
        let mut rows = stmt.query(params![user.get(), now])?;
        let mut devices = Vec::new();
        while let Some(row) = rows.next()? {
            devices.push(device_of(row)?);
        }
        Ok(devices)
    }

    /// Who `device` signs in as, if it is neither revoked nor expired.
    ///
    /// # Errors
    /// A database error, or a stored row that does not parse.
    pub fn device_session(&self, device: DeviceId) -> Result<Option<DeviceSession>, StoreError> {
        let now = sql_ts(self.clock.now_ms());
        let conn = lock(&self.conn);
        let found: Option<(i64, String)> = conn
            .query_row(
                "SELECT d.user_id, u.role FROM devices d JOIN users u ON u.id = d.user_id
                 WHERE d.id = ?1 AND d.revoked_at IS NULL AND d.expires_at > ?2",
                params![device.get(), now],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        found
            .map(|(user, role)| {
                Ok(DeviceSession {
                    device,
                    user: UserId::from_stored(user),
                    role: super::users::role_of(user, &role)?,
                })
            })
            .transpose()
    }

    /// Records that `device` was used now and keeps it valid until `expires_at`.
    ///
    /// # Errors
    /// A database error.
    pub fn touch_device(&self, device: DeviceId, expires_at: u64) -> Result<(), StoreError> {
        lock(&self.conn).execute(
            "UPDATE devices SET last_seen_at = ?2, expires_at = max(expires_at, ?3)
             WHERE id = ?1 AND revoked_at IS NULL",
            params![
                device.get(),
                sql_ts(self.clock.now_ms()),
                sql_ts(expires_at)
            ],
        )?;
        Ok(())
    }

    /// Revokes `user`'s `device`. `false` when it is not theirs or is gone already.
    ///
    /// # Errors
    /// A database error.
    pub fn revoke_device(&self, user: UserId, device: DeviceId) -> Result<bool, StoreError> {
        let changed = lock(&self.conn).execute(
            "UPDATE devices SET revoked_at = ?3
             WHERE id = ?1 AND user_id = ?2 AND revoked_at IS NULL",
            params![device.get(), user.get(), sql_ts(self.clock.now_ms())],
        )?;
        if changed == 0 {
            return Ok(false);
        }
        self.forget_session(device)?;
        Ok(true)
    }

    /// Revokes every device of `user` except `keep` (a password change keeps the browser that
    /// made it; a reset keeps none), the ids revoked.
    ///
    /// # Errors
    /// A database error.
    pub fn revoke_devices_of(
        &self,
        user: UserId,
        keep: Option<DeviceId>,
    ) -> Result<Vec<DeviceId>, StoreError> {
        let revoked: Vec<DeviceId> = {
            let conn = lock(&self.conn);
            let mut stmt = conn.prepare(
                "UPDATE devices SET revoked_at = ?2
                 WHERE user_id = ?1 AND revoked_at IS NULL AND id IS NOT ?3
                 RETURNING id",
            )?;
            stmt.query_map(
                params![
                    user.get(),
                    sql_ts(self.clock.now_ms()),
                    keep.map(DeviceId::get)
                ],
                |row| row.get::<_, i64>(0),
            )
            .and_then(Iterator::collect::<Result<Vec<i64>, _>>)?
            .into_iter()
            .map(DeviceId::from_stored)
            .collect()
        };
        for id in &revoked {
            self.forget_session(*id)?;
        }
        Ok(revoked)
    }

    /// A revoked device can't refresh any more: its refresh digest goes.
    fn forget_session(&self, device: DeviceId) -> Result<(), StoreError> {
        lock(&self.conn).execute(
            "DELETE FROM account_sessions WHERE device_id = ?1",
            [device.get()],
        )?;
        Ok(())
    }

    /// Which sign-in methods exist and whether each is on.
    ///
    /// # Errors
    /// A database error, or a stored row that does not parse.
    pub fn sign_in_methods(&self) -> Result<Vec<(SignInMethod, bool)>, StoreError> {
        let conn = lock(&self.conn);
        let mut stmt =
            conn.prepare("SELECT method, enabled FROM sign_in_methods ORDER BY method")?;
        let mut rows = stmt.query([])?;
        let mut methods = Vec::new();
        while let Some(row) = rows.next()? {
            let name: String = row.get(0)?;
            let method = SignInMethod::parse(&name)
                .ok_or_else(|| corrupt("sign_in_methods", 0, "unknown method"))?;
            methods.push((method, row.get::<_, i64>(1)? == 1));
        }
        Ok(methods)
    }

    /// Makes `method` exist, switched on (passkeys do this when the first one is enrolled). One
    /// that exists already is left as it is.
    ///
    /// # Errors
    /// A database error.
    pub fn register_sign_in_method(&self, method: SignInMethod) -> Result<(), StoreError> {
        lock(&self.conn).execute(
            "INSERT INTO sign_in_methods (method, enabled) VALUES (?1, 1) ON CONFLICT DO NOTHING",
            [method.as_str()],
        )?;
        Ok(())
    }

    /// Switches `method` on or off. The last method that is on can't be switched off, and a
    /// method that does not exist yet can't be switched at all.
    ///
    /// # Errors
    /// [`StoreError::SignInMethod`] saying why, or a database error.
    pub fn set_sign_in_method(
        &self,
        method: SignInMethod,
        enabled: bool,
    ) -> Result<(), StoreError> {
        let mut conn = lock(&self.conn);
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let (exists, others): (bool, i64) = tx.query_row(
            "SELECT coalesce(max(method = ?1), 0), coalesce(sum(enabled = 1 AND method <> ?1), 0)
             FROM sign_in_methods",
            [method.as_str()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if !exists {
            return Err(StoreError::SignInMethod(format!(
                "{} sign-in doesn't exist yet",
                method.as_str()
            )));
        }
        if !enabled && others == 0 {
            return Err(StoreError::SignInMethod(
                "at least one way of signing in must stay on".to_owned(),
            ));
        }
        tx.execute(
            "UPDATE sign_in_methods SET enabled = ?2 WHERE method = ?1",
            params![method.as_str(), i64::from(enabled)],
        )?;
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
