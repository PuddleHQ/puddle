// SPDX-License-Identifier: GPL-3.0-or-later
//! The "decrypt every host" switch of each workspace and the names that are never decrypted.

use puddle_types::{Event, WorkspaceName};
use rusqlite::{OptionalExtension, Transaction, params};

use super::{Store, lock};
use crate::error::StoreError;

/// A workspace's own settings for decrypting every host. The global never-decrypt names are
/// separate ([`Store::global_never_decrypt`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkspaceInterception {
    /// Decrypt every host except the never-decrypt names (off by default).
    pub decrypt_all: bool,
    /// The names this workspace never decrypts, as written (`example.com`, `*.example.com`).
    pub never_decrypt: Vec<String>,
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
fn changed(fx: &mut Vec<Event>, workspace: &WorkspaceName) {
    // The event that makes the host recompute what a running workspace decrypts.
    fx.push(Event::WorkspaceGitChanged {
        workspace: workspace.clone(),
    });
}

impl Store {
    /// `workspace`'s switch and its own never-decrypt names.
    ///
    /// # Errors
    /// A database error.
    pub fn workspace_interception(
        &self,
        workspace: &WorkspaceName,
    ) -> Result<WorkspaceInterception, StoreError> {
        let conn = lock(&self.conn);
        let decrypt_all = conn
            .query_row(
                "SELECT decrypt_all FROM workspace_interception WHERE workspace_id = ?1",
                [workspace.as_str()],
                |r| r.get::<_, bool>(0),
            )
            .optional()?
            .unwrap_or(false);
        let mut stmt = conn.prepare(
            "SELECT pattern FROM never_decrypt WHERE workspace_id = ?1 ORDER BY pattern",
        )?;
        let never_decrypt = stmt
            .query_map([workspace.as_str()], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(WorkspaceInterception {
            decrypt_all,
            never_decrypt,
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

    /// The time and size the learned pass-through lists use (the built-in values until saved).
    ///
    /// # Errors
    /// A database error.
    pub fn learned_limits(&self) -> Result<LearnedLimits, StoreError> {
        let conn = lock(&self.conn);
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

    /// Saves the learned-list limits (both greater than zero; the caller has checked the range)
    /// and tells every workspace that decrypts every host, so a running one applies them at once.
    ///
    /// # Errors
    /// A database error, or a zero value; nothing changes then.
    pub fn set_learned_limits(&self, limits: LearnedLimits) -> Result<(), StoreError> {
        let before = self.learned_limits()?;
        self.change(|tx, _now, fx| {
            tx.execute(
                "INSERT INTO learned_limits (id, ttl_secs, cap) VALUES (1, ?1, ?2)
                 ON CONFLICT (id) DO UPDATE SET ttl_secs = ?1, cap = ?2",
                params![limits.ttl_secs, limits.cap],
            )?;
            if before != limits {
                let mut stmt = tx.prepare(
                    "SELECT workspace_id FROM workspace_interception WHERE decrypt_all = 1",
                )?;
                let names = stmt.query_map([], |r| r.get::<_, String>(0))?;
                for name in names.flatten() {
                    if let Ok(workspace) = WorkspaceName::new(&name) {
                        changed(fx, &workspace);
                    }
                }
            }
            Ok(())
        })
    }

    /// Turns "decrypt every host" on or off for `workspace`. An unchanged value sends no event.
    ///
    /// # Errors
    /// A database error; nothing changes then.
    pub fn set_decrypt_all(&self, workspace: &WorkspaceName, on: bool) -> Result<(), StoreError> {
        let before = self.workspace_interception(workspace)?.decrypt_all;
        self.change(|tx, _now, fx| {
            tx.execute(
                "INSERT INTO workspace_interception (workspace_id, decrypt_all) VALUES (?1, ?2)
                 ON CONFLICT (workspace_id) DO UPDATE SET decrypt_all = ?2",
                params![workspace.as_str(), on],
            )?;
            if before != on {
                changed(fx, workspace);
            }
            Ok(())
        })
    }

    /// Replaces `workspace`'s own never-decrypt names with `patterns` (the caller has checked
    /// them).
    ///
    /// # Errors
    /// A database error; nothing changes then.
    pub fn set_never_decrypt(
        &self,
        workspace: &WorkspaceName,
        patterns: &[String],
    ) -> Result<(), StoreError> {
        let before = self.workspace_interception(workspace)?.never_decrypt;
        self.change(|tx, _now, fx| {
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
        })
    }

    /// Replaces the names no workspace decrypts with `patterns`, and tells every workspace that
    /// has a switch row (the others do not decrypt every host, so it changes nothing for them).
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
            let mut stmt = tx
                .prepare("SELECT workspace_id FROM workspace_interception WHERE decrypt_all = 1")?;
            let names = stmt.query_map([], |r| r.get::<_, String>(0))?;
            for name in names.flatten() {
                if let Ok(workspace) = WorkspaceName::new(&name) {
                    changed(fx, &workspace);
                }
            }
            Ok(())
        })
    }
}

/// Removes what a deleted workspace held. The workspace's own change event covers it.
pub(super) fn delete_workspace_rows(
    tx: &Transaction<'_>,
    workspace: &WorkspaceName,
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
mod tests {
    use std::sync::{Arc, Mutex};

    use puddle_types::EventSink;

    use super::*;
    use crate::clock::ManualClock;
    use crate::store::Limits;

    #[derive(Default)]
    struct Seen(Mutex<Vec<Event>>);

    impl EventSink for Seen {
        fn emit(&self, event: Event) {
            self.0.lock().unwrap().push(event);
        }
    }

    fn ws(text: &str) -> WorkspaceName {
        WorkspaceName::new(text).unwrap()
    }

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    fn store() -> (Store, Arc<Seen>) {
        let seen = Arc::new(Seen::default());
        let store = Store::open_in_memory(Arc::new(ManualClock::new(1)), Limits::default())
            .unwrap()
            .with_events(Arc::clone(&seen) as Arc<dyn EventSink>);
        (store, seen)
    }

    fn changes(seen: &Seen) -> usize {
        seen.0
            .lock()
            .unwrap()
            .iter()
            .filter(|e| matches!(e, Event::WorkspaceGitChanged { .. }))
            .count()
    }

    #[test]
    fn a_workspace_decrypts_nothing_extra_until_its_switch_is_turned_on() {
        let (store, seen) = store();
        assert_eq!(
            store.workspace_interception(&ws("a")).unwrap(),
            WorkspaceInterception::default()
        );
        store.set_decrypt_all(&ws("a"), true).unwrap();
        store.set_decrypt_all(&ws("a"), true).unwrap();
        assert!(store.workspace_interception(&ws("a")).unwrap().decrypt_all);
        assert!(!store.workspace_interception(&ws("b")).unwrap().decrypt_all);
        assert_eq!(changes(&seen), 1, "an unchanged value is not a change");
        store.set_decrypt_all(&ws("a"), false).unwrap();
        assert!(!store.workspace_interception(&ws("a")).unwrap().decrypt_all);
        assert_eq!(changes(&seen), 2);
    }

    #[test]
    fn never_decrypt_names_are_per_workspace_sorted_and_replaced_as_a_whole() {
        let (store, seen) = store();
        store
            .set_never_decrypt(
                &ws("a"),
                &strings(&["b.example", "*.a.example", "b.example"]),
            )
            .unwrap();
        assert_eq!(
            store
                .workspace_interception(&ws("a"))
                .unwrap()
                .never_decrypt,
            ["*.a.example", "b.example"]
        );
        assert_eq!(
            store
                .workspace_interception(&ws("b"))
                .unwrap()
                .never_decrypt,
            strings(&[])
        );
        assert_eq!(changes(&seen), 1);
        store
            .set_never_decrypt(&ws("a"), &strings(&["*.a.example", "b.example"]))
            .unwrap();
        assert_eq!(changes(&seen), 1, "the same names are not a change");
        store.set_never_decrypt(&ws("a"), &[]).unwrap();
        assert_eq!(
            store
                .workspace_interception(&ws("a"))
                .unwrap()
                .never_decrypt,
            strings(&[])
        );
        assert_eq!(changes(&seen), 2);
    }

    #[test]
    fn the_global_list_tells_only_the_workspaces_that_decrypt_every_host() {
        let (store, seen) = store();
        store.set_decrypt_all(&ws("on"), true).unwrap();
        store.set_decrypt_all(&ws("off"), false).unwrap();
        let before = changes(&seen);
        store
            .set_global_never_decrypt(&strings(&["bank.example"]))
            .unwrap();
        assert_eq!(store.global_never_decrypt().unwrap(), ["bank.example"]);
        assert_eq!(changes(&seen) - before, 1);
        store.set_global_never_decrypt(&[]).unwrap();
        assert_eq!(store.global_never_decrypt().unwrap(), strings(&[]));
    }

    #[test]
    fn the_learned_limits_are_twelve_hours_and_1024_until_saved_and_tell_the_decrypting_workspaces()
    {
        let (store, seen) = store();
        let built_in = LearnedLimits::default();
        assert_eq!((built_in.ttl_secs, built_in.cap), (43_200, 1024));
        assert_eq!(store.learned_limits().unwrap(), built_in);
        store.set_decrypt_all(&ws("on"), true).unwrap();
        store.set_decrypt_all(&ws("off"), false).unwrap();
        let before = changes(&seen);
        let mine = LearnedLimits {
            ttl_secs: 600,
            cap: 8,
        };
        store.set_learned_limits(mine).unwrap();
        assert_eq!(store.learned_limits().unwrap(), mine);
        assert_eq!(
            changes(&seen) - before,
            1,
            "only the workspace that decrypts"
        );
        store.set_learned_limits(mine).unwrap();
        assert_eq!(
            changes(&seen) - before,
            1,
            "the same values are not a change"
        );
        store.set_learned_limits(built_in).unwrap();
        assert_eq!(store.learned_limits().unwrap(), built_in);
        assert!(
            store
                .set_learned_limits(LearnedLimits {
                    ttl_secs: 0,
                    cap: 1
                })
                .is_err(),
            "the table refuses zero"
        );
    }

    #[test]
    fn deleting_a_workspace_deletes_its_settings_and_keeps_the_global_list() {
        let (store, _) = store();
        store.set_decrypt_all(&ws("a"), true).unwrap();
        store
            .set_never_decrypt(&ws("a"), &strings(&["x.example"]))
            .unwrap();
        store
            .set_global_never_decrypt(&strings(&["g.example"]))
            .unwrap();
        store.delete_workspace(&ws("a")).unwrap();
        assert_eq!(
            store.workspace_interception(&ws("a")).unwrap(),
            WorkspaceInterception::default()
        );
        assert_eq!(store.global_never_decrypt().unwrap(), ["g.example"]);
    }
}
