// SPDX-License-Identifier: AGPL-3.0-or-later
//! The "decrypt every host" switch of each workspace and the names that are never decrypted.

use puddle_types::{Event, InterceptionMode, WorkspaceId};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

use super::{Store, lock};
use crate::error::StoreError;
use crate::users::UserId;

/// A workspace's own settings for decrypting port 443. The global setting
/// ([`Store::interception_settings`]) and the global never-decrypt names
/// ([`Store::global_never_decrypt`]) are separate.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkspaceInterception {
    /// This workspace's own mode; `None` follows the global setting.
    pub mode: Option<InterceptionMode>,
    /// The names this workspace never decrypts, as written (`example.com`, `*.example.com`).
    pub never_decrypt: Vec<String>,
}

/// The global setting for every workspace that does not choose its own mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InterceptionSettings {
    /// The mode the user chose; `None` until they do (an existing install then keeps
    /// [`InterceptionMode::EXISTING_INSTALL`]).
    pub mode: Option<InterceptionMode>,
    /// Whether a host that refuses Puddle's certificate is passed through for a while and put on
    /// the learned list (on unless turned off).
    pub learn_from_rejections: bool,
    /// Whether the user has seen the notice that offers the choice (existing installs).
    pub notice_seen: bool,
}

impl Default for InterceptionSettings {
    fn default() -> Self {
        Self {
            mode: None,
            learn_from_rejections: true,
            notice_seen: false,
        }
    }
}

impl InterceptionSettings {
    /// The mode in force for a workspace that follows the global setting.
    #[must_use]
    pub fn effective_mode(self) -> InterceptionMode {
        self.mode.unwrap_or(InterceptionMode::EXISTING_INSTALL)
    }
}

/// How long a host that refused Puddle's certificate stays passed through: 12 hours.
pub const DEFAULT_LEARNED_TTL_SECS: u32 = 12 * 3600;

/// How many such hosts a workspace remembers: 1024.
pub const DEFAULT_LEARNED_CAP: u32 = 1024;

/// How long, and how many, hosts that refused Puddle's certificate are passed through. One pair
/// for every workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LearnedLimits {
    /// Seconds a host stays passed through after the refusal.
    pub ttl_secs: u32,
    /// Most hosts one workspace remembers; the oldest makes room.
    pub cap: u32,
}

impl Default for LearnedLimits {
    fn default() -> Self {
        Self {
            ttl_secs: DEFAULT_LEARNED_TTL_SECS,
            cap: DEFAULT_LEARNED_CAP,
        }
    }
}

/// Tells the running workspace to read what it decrypts again.
fn changed(fx: &mut Vec<Event>, workspace: &WorkspaceId) {
    // The event that makes the host recompute what a running workspace decrypts.
    fx.push(Event::WorkspaceGitChanged {
        workspace: workspace.clone(),
    });
}

/// Tells every workspace the store knows (a global change reaches the ones that inherit it).
fn changed_everywhere(tx: &Transaction<'_>, fx: &mut Vec<Event>) -> Result<(), StoreError> {
    let mut stmt = tx.prepare("SELECT workspace_id FROM workspace_owners")?;
    let names = stmt.query_map([], |r| r.get::<_, String>(0))?;
    for name in names.flatten() {
        if let Ok(workspace) = WorkspaceId::new(&name) {
            changed(fx, &workspace);
        }
    }
    Ok(())
}

/// `workspace`'s switch and names as `conn` sees them; a setter reads them inside its own write
/// transaction so the change it reports is against what it replaces.
fn workspace_interception_in(
    conn: &Connection,
    workspace: &WorkspaceId,
) -> Result<WorkspaceInterception, StoreError> {
    let mode = conn
        .query_row(
            "SELECT mode FROM workspace_interception WHERE workspace_id = ?1",
            [workspace.as_str()],
            |r| r.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten()
        .and_then(|name| InterceptionMode::parse(&name));
    let mut stmt =
        conn.prepare("SELECT pattern FROM never_decrypt WHERE workspace_id = ?1 ORDER BY pattern")?;
    let never_decrypt = stmt
        .query_map([workspace.as_str()], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(WorkspaceInterception {
        mode,
        never_decrypt,
    })
}

fn interception_settings_in(conn: &Connection) -> Result<InterceptionSettings, StoreError> {
    Ok(conn
        .query_row(
            "SELECT mode, learn, notice_seen FROM interception_settings WHERE id = 1",
            [],
            |r| {
                Ok(InterceptionSettings {
                    mode: r
                        .get::<_, Option<String>>(0)?
                        .and_then(|name| InterceptionMode::parse(&name)),
                    learn_from_rejections: r.get(1)?,
                    notice_seen: r.get(2)?,
                })
            },
        )
        .optional()?
        .unwrap_or_default())
}

fn learned_limits_in(conn: &Connection) -> Result<LearnedLimits, StoreError> {
    Ok(conn
        .query_row(
            "SELECT ttl_secs, cap FROM learned_limits WHERE id = 1",
            [],
            |r| {
                Ok(LearnedLimits {
                    ttl_secs: r.get(0)?,
                    cap: r.get(1)?,
                })
            },
        )
        .optional()?
        .unwrap_or_default())
}

fn set_settings_in(
    tx: &Transaction<'_>,
    fx: &mut Vec<Event>,
    settings: InterceptionSettings,
) -> Result<(), StoreError> {
    let before = interception_settings_in(tx)?;
    tx.execute(
        "INSERT INTO interception_settings (id, mode, learn, notice_seen)
         VALUES (1, ?1, ?2, ?3)
         ON CONFLICT (id) DO UPDATE SET mode = ?1, learn = ?2, notice_seen = ?3",
        params![
            settings.mode.map(InterceptionMode::as_str),
            settings.learn_from_rejections,
            settings.notice_seen
        ],
    )?;
    if before.mode != settings.mode
        || before.learn_from_rejections != settings.learn_from_rejections
    {
        changed_everywhere(tx, fx)?;
    }
    Ok(())
}

fn set_mode_in(
    tx: &Transaction<'_>,
    fx: &mut Vec<Event>,
    workspace: &WorkspaceId,
    mode: Option<InterceptionMode>,
) -> Result<(), StoreError> {
    let before = workspace_interception_in(tx, workspace)?.mode;
    tx.execute(
        "INSERT INTO workspace_interception (workspace_id, mode) VALUES (?1, ?2)
         ON CONFLICT (workspace_id) DO UPDATE SET mode = ?2",
        params![workspace.as_str(), mode.map(InterceptionMode::as_str)],
    )?;
    if before != mode {
        changed(fx, workspace);
    }
    Ok(())
}

fn set_names_in(
    tx: &Transaction<'_>,
    fx: &mut Vec<Event>,
    workspace: &WorkspaceId,
    patterns: &[String],
) -> Result<(), StoreError> {
    let before = workspace_interception_in(tx, workspace)?.never_decrypt;
    tx.execute(
        "DELETE FROM never_decrypt WHERE workspace_id = ?1",
        [workspace.as_str()],
    )?;
    for pattern in patterns {
        tx.execute(
            "INSERT OR IGNORE INTO never_decrypt (workspace_id, pattern) VALUES (?1, ?2)",
            params![workspace.as_str(), pattern],
        )?;
    }
    let mut now: Vec<&String> = patterns.iter().collect();
    now.sort();
    now.dedup();
    if now.len() != before.len() || now.iter().zip(&before).any(|(a, b)| *a != b) {
        changed(fx, workspace);
    }
    Ok(())
}

impl Store {
    /// `workspace`'s switch and its own never-decrypt names.
    ///
    /// # Errors
    /// A database error.
    pub fn workspace_interception(
        &self,
        workspace: &WorkspaceId,
    ) -> Result<WorkspaceInterception, StoreError> {
        workspace_interception_in(&lock(&self.conn), workspace)
    }

    /// The global interception setting (the defaults until saved).
    ///
    /// # Errors
    /// A database error.
    pub fn interception_settings(&self) -> Result<InterceptionSettings, StoreError> {
        interception_settings_in(&lock(&self.conn))
    }

    /// Saves the global interception setting and tells every workspace when it changes what they
    /// decrypt (the mode or the learning switch). The notice flag alone sends no event.
    ///
    /// # Errors
    /// A database error; nothing changes then.
    pub fn set_interception_settings(
        &self,
        settings: InterceptionSettings,
    ) -> Result<(), StoreError> {
        self.change(|tx, _now, fx| set_settings_in(tx, fx, settings))
    }

    /// Changes only the parts of the global interception setting that are `Some`, read and written
    /// in one transaction, so two overlapping patches of different parts both stay. Choosing a mode
    /// answers the notice (`notice_seen` becomes true) unless `notice_seen` says otherwise. The
    /// events are those of [`Store::set_interception_settings`]. Returns the setting now.
    ///
    /// # Errors
    /// A database error; nothing changes then.
    pub fn patch_interception_settings(
        &self,
        mode: Option<InterceptionMode>,
        learn_from_rejections: Option<bool>,
        notice_seen: Option<bool>,
    ) -> Result<InterceptionSettings, StoreError> {
        self.change(|tx, _now, fx| {
            let mut saved = interception_settings_in(tx)?;
            if let Some(mode) = mode {
                saved.mode = Some(mode);
                saved.notice_seen = true;
            }
            if let Some(on) = learn_from_rejections {
                saved.learn_from_rejections = on;
            }
            if let Some(seen) = notice_seen {
                saved.notice_seen = seen;
            }
            set_settings_in(tx, fx, saved)?;
            Ok(saved)
        })
    }

    /// The names no workspace decrypts, whatever its switch.
    ///
    /// # Errors
    /// A database error.
    pub fn global_never_decrypt(&self) -> Result<Vec<String>, StoreError> {
        let conn = lock(&self.conn);
        let mut stmt = conn.prepare("SELECT pattern FROM never_decrypt_global ORDER BY pattern")?;
        Ok(stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?)
    }

    /// The never-decrypt names `user` sees as (workspace, name): the global names (workspace
    /// `None`) and the own names of each workspace of `user`, ordered global first, then by
    /// workspace and name. Another user's workspaces and their names are left out.
    ///
    /// # Errors
    /// A database error.
    pub fn never_decrypt_names(
        &self,
        user: UserId,
    ) -> Result<Vec<(Option<String>, String)>, StoreError> {
        let conn = lock(&self.conn);
        let mut stmt = conn.prepare(
            "SELECT NULL, pattern FROM never_decrypt_global
             UNION ALL
             SELECT n.workspace_id, n.pattern FROM never_decrypt n
               JOIN workspace_owners o ON o.workspace_id = n.workspace_id
              WHERE o.user_id = ?1
             ORDER BY 1, 2",
        )?;
        Ok(stmt
            .query_map([user.get()], |r| {
                Ok((r.get::<_, Option<String>>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?)
    }

    /// The time and size the learned pass-through lists use (the built-in values until saved).
    ///
    /// # Errors
    /// A database error.
    pub fn learned_limits(&self) -> Result<LearnedLimits, StoreError> {
        learned_limits_in(&lock(&self.conn))
    }

    /// Saves the learned-list limits (both greater than zero; the caller has checked the range)
    /// and tells every workspace that decrypts every host, so a running one applies them at once.
    ///
    /// # Errors
    /// A database error, or a zero value; nothing changes then.
    pub fn set_learned_limits(&self, limits: LearnedLimits) -> Result<(), StoreError> {
        self.change(|tx, _now, fx| {
            let before = learned_limits_in(tx)?;
            tx.execute(
                "INSERT INTO learned_limits (id, ttl_secs, cap) VALUES (1, ?1, ?2)
                 ON CONFLICT (id) DO UPDATE SET ttl_secs = ?1, cap = ?2",
                params![limits.ttl_secs, limits.cap],
            )?;
            if before != limits {
                changed_everywhere(tx, fx)?;
            }
            Ok(())
        })
    }

    /// Sets `workspace`'s own mode (`None` follows the global setting). An unchanged value sends
    /// no event.
    ///
    /// # Errors
    /// A database error; nothing changes then.
    pub fn set_workspace_mode(
        &self,
        workspace: &WorkspaceId,
        mode: Option<InterceptionMode>,
    ) -> Result<(), StoreError> {
        self.change(|tx, _now, fx| set_mode_in(tx, fx, workspace, mode))
    }

    /// Replaces `workspace`'s own never-decrypt names with `patterns` (the caller has checked
    /// them).
    ///
    /// # Errors
    /// A database error; nothing changes then.
    pub fn set_never_decrypt(
        &self,
        workspace: &WorkspaceId,
        patterns: &[String],
    ) -> Result<(), StoreError> {
        self.change(|tx, _now, fx| set_names_in(tx, fx, workspace, patterns))
    }

    /// Changes `workspace`'s own mode (`Some(None)` follows the global setting) and/or replaces
    /// its never-decrypt names, each only when given, in one transaction: a concurrent change of
    /// the other part is never lost and no half of the patch is saved alone. The settings now.
    ///
    /// # Errors
    /// A database error; nothing changes then.
    pub fn patch_workspace_interception(
        &self,
        workspace: &WorkspaceId,
        mode: Option<Option<InterceptionMode>>,
        never_decrypt: Option<&[String]>,
    ) -> Result<WorkspaceInterception, StoreError> {
        self.change(|tx, _now, fx| {
            if let Some(mode) = mode {
                set_mode_in(tx, fx, workspace, mode)?;
            }
            if let Some(names) = never_decrypt {
                set_names_in(tx, fx, workspace, names)?;
            }
            workspace_interception_in(tx, workspace)
        })
    }

    /// Adds one name to `workspace`'s own never-decrypt names in one transaction, so adds that
    /// overlap with each other or with a read never drop a name. `Ok(false)` when the list
    /// already holds `max` other names; a name already on the list is `Ok(true)`.
    ///
    /// # Errors
    /// A database error; nothing changes then.
    pub fn add_never_decrypt(
        &self,
        workspace: &WorkspaceId,
        pattern: &str,
        max: usize,
    ) -> Result<bool, StoreError> {
        self.change(|tx, _now, fx| {
            let held: i64 = scalar(tx, COUNT_OWN, [workspace.as_str()])?;
            let added = tx.execute(
                "INSERT OR IGNORE INTO never_decrypt (workspace_id, pattern)
                 SELECT ?1, ?2 WHERE ?3 > ?4",
                params![workspace.as_str(), pattern, max_names(max), held],
            )?;
            if added == 1 {
                changed(fx, workspace);
            }
            let present: bool = scalar(tx, HOLDS_OWN, [workspace.as_str(), pattern])?;
            Ok(present)
        })
    }

    /// Adds one name to the names no workspace decrypts, as [`Self::add_never_decrypt`] does for
    /// one workspace, and tells every workspace when the list grew.
    ///
    /// # Errors
    /// A database error; nothing changes then.
    pub fn add_global_never_decrypt(&self, pattern: &str, max: usize) -> Result<bool, StoreError> {
        self.change(|tx, _now, fx| {
            let held: i64 = scalar(tx, COUNT_GLOBAL, [])?;
            let added = tx.execute(
                "INSERT OR IGNORE INTO never_decrypt_global (pattern) SELECT ?1 WHERE ?2 > ?3",
                params![pattern, max_names(max), held],
            )?;
            if added == 1 {
                changed_everywhere(tx, fx)?;
            }
            let present: bool = scalar(tx, HOLDS_GLOBAL, [pattern])?;
            Ok(present)
        })
    }

    /// Replaces the names no workspace decrypts with `patterns`, and tells every workspace.
    ///
    /// # Errors
    /// A database error; nothing changes then.
    pub fn set_global_never_decrypt(&self, patterns: &[String]) -> Result<(), StoreError> {
        self.change(|tx, _now, fx| {
            tx.execute("DELETE FROM never_decrypt_global", [])?;
            for pattern in patterns {
                tx.execute(
                    "INSERT OR IGNORE INTO never_decrypt_global (pattern) VALUES (?1)",
                    [pattern],
                )?;
            }
            changed_everywhere(tx, fx)?;
            Ok(())
        })
    }
}

const COUNT_OWN: &str = "SELECT COUNT(*) FROM never_decrypt WHERE workspace_id = ?1";
const COUNT_GLOBAL: &str = "SELECT COUNT(*) FROM never_decrypt_global";
const HOLDS_OWN: &str =
    "SELECT EXISTS (SELECT 1 FROM never_decrypt WHERE workspace_id = ?1 AND pattern = ?2)";
const HOLDS_GLOBAL: &str = "SELECT EXISTS (SELECT 1 FROM never_decrypt_global WHERE pattern = ?1)";

/// The one value a query selects.
fn scalar<T: rusqlite::types::FromSql>(
    tx: &Transaction<'_>,
    sql: &str,
    params: impl rusqlite::Params,
) -> Result<T, StoreError> {
    Ok(tx.query_row(sql, params, |r| r.get(0))?)
}

/// A list limit as SQLite holds integers.
fn max_names(max: usize) -> i64 {
    i64::try_from(max).unwrap_or(i64::MAX)
}

/// Removes what a deleted workspace held. The workspace's own change event covers it.
pub(super) fn delete_workspace_rows(
    tx: &Transaction<'_>,
    workspace: &WorkspaceId,
) -> Result<(), StoreError> {
    tx.execute(
        "DELETE FROM workspace_interception WHERE workspace_id = ?1",
        [workspace.as_str()],
    )?;
    tx.execute(
        "DELETE FROM never_decrypt WHERE workspace_id = ?1",
        [workspace.as_str()],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests;
