// SPDX-License-Identifier: GPL-3.0-or-later
//! Shell init rows in the store.

use puddle_types::{Event, WorkspaceName};
use rusqlite::{OptionalExtension, Transaction, params};

use super::{Store, lock, sql_ts};
use crate::error::StoreError;
use crate::shell_init::{ShellInit, ShellInitText};

fn changed(fx: &mut Vec<Event>, workspace: &WorkspaceName) {
    fx.push(Event::WorkspaceShellInitChanged {
        workspace: workspace.clone(),
    });
}

impl Store {
    /// The shell init saved for `workspace`, if it has one.
    ///
    /// # Errors
    /// A database error, or a stored row that does not parse.
    pub fn shell_init(&self, workspace: &WorkspaceName) -> Result<Option<ShellInit>, StoreError> {
        let conn = lock(&self.conn);
        let row: Option<(String, i64)> = conn
            .query_row(
                "SELECT content, changed_at FROM shell_init WHERE workspace_id = ?1",
                [workspace.as_str()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        row.map(|(content, changed_at)| {
            Ok(ShellInit {
                content: ShellInitText::new(&content).map_err(|e| StoreError::Corrupt {
                    table: "shell_init",
                    id: 0,
                    reason: e.to_string(),
                })?,
                changed_at: u64::try_from(changed_at).unwrap_or(0),
            })
        })
        .transpose()
    }

    /// Saves `content` as `workspace`'s shell init, replacing what was there. An unchanged text
    /// still counts as a save (it carries a new time) but sends no event.
    ///
    /// # Errors
    /// A database error; nothing changes then.
    pub fn set_shell_init(
        &self,
        workspace: &WorkspaceName,
        content: &ShellInitText,
    ) -> Result<ShellInit, StoreError> {
        let before = self.shell_init(workspace)?;
        let unchanged = before.as_ref().is_some_and(|b| &b.content == content);
        let changed_at = self.change(|tx, now, fx| {
            tx.execute(
                "INSERT INTO shell_init (workspace_id, content, changed_at) VALUES (?1, ?2, ?3)
                 ON CONFLICT (workspace_id) DO UPDATE SET content = ?2, changed_at = ?3",
                params![workspace.as_str(), content.as_str(), sql_ts(now)],
            )?;
            if !unchanged {
                changed(fx, workspace);
            }
            Ok(now)
        })?;
        Ok(ShellInit {
            content: content.clone(),
            changed_at,
        })
    }

    /// Removes `workspace`'s shell init. Returns whether it had one.
    ///
    /// # Errors
    /// A database error; nothing changes then.
    pub fn clear_shell_init(&self, workspace: &WorkspaceName) -> Result<bool, StoreError> {
        self.change(|tx, _now, fx| {
            let had = tx.execute(
                "DELETE FROM shell_init WHERE workspace_id = ?1",
                [workspace.as_str()],
            )? > 0;
            if had {
                changed(fx, workspace);
            }
            Ok(had)
        })
    }
}

/// Removes what a deleted workspace held. The workspace's own change event covers it (its page is
/// gone), so none is sent here.
pub(super) fn delete_workspace_rows(
    tx: &Transaction<'_>,
    workspace: &WorkspaceName,
) -> Result<(), StoreError> {
    tx.execute(
        "DELETE FROM shell_init WHERE workspace_id = ?1",
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

    fn text(t: &str) -> ShellInitText {
        ShellInitText::new(t).unwrap()
    }

    fn store_with_events() -> (Store, Arc<Seen>) {
        let seen = Arc::new(Seen::default());
        let store = Store::open_in_memory(Arc::new(ManualClock::new(5)), Limits::default())
            .unwrap()
            .with_events(Arc::clone(&seen) as Arc<dyn EventSink>);
        (store, seen)
    }

    #[test]
    fn a_shell_init_is_kept_per_workspace_and_replaced_by_a_new_save() {
        let (store, seen) = store_with_events();
        assert_eq!(store.shell_init(&ws("a")).unwrap(), None);
        let saved = store.set_shell_init(&ws("a"), &text("export A=1")).unwrap();
        assert_eq!(saved.content.as_str(), "export A=1\n");
        assert_eq!(saved.changed_at, 5);
        store.set_shell_init(&ws("b"), &text("export B=1")).unwrap();
        store.set_shell_init(&ws("a"), &text("export A=2")).unwrap();
        assert_eq!(
            store
                .shell_init(&ws("a"))
                .unwrap()
                .unwrap()
                .content
                .as_str(),
            "export A=2\n"
        );
        assert_eq!(
            store
                .shell_init(&ws("b"))
                .unwrap()
                .unwrap()
                .content
                .as_str(),
            "export B=1\n"
        );
        assert_eq!(seen.0.lock().unwrap().len(), 3);
    }

    #[test]
    fn saving_the_same_text_again_sends_no_event_and_removing_one_that_is_not_there_neither() {
        let (store, seen) = store_with_events();
        store.set_shell_init(&ws("a"), &text("x")).unwrap();
        store.set_shell_init(&ws("a"), &text("x")).unwrap();
        assert_eq!(seen.0.lock().unwrap().len(), 1);
        assert!(!store.clear_shell_init(&ws("none")).unwrap());
        assert!(store.clear_shell_init(&ws("a")).unwrap());
        assert_eq!(store.shell_init(&ws("a")).unwrap(), None);
        assert_eq!(
            *seen.0.lock().unwrap(),
            [
                Event::WorkspaceShellInitChanged { workspace: ws("a") },
                Event::WorkspaceShellInitChanged { workspace: ws("a") },
            ]
        );
    }

    #[test]
    fn deleting_a_workspace_takes_its_shell_init_and_no_other() {
        let (store, _) = store_with_events();
        store.set_shell_init(&ws("a"), &text("x")).unwrap();
        store.set_shell_init(&ws("b"), &text("y")).unwrap();
        store.delete_workspace(&ws("a")).unwrap();
        assert_eq!(store.shell_init(&ws("a")).unwrap(), None);
        assert!(store.shell_init(&ws("b")).unwrap().is_some());
    }

    #[test]
    fn a_row_edited_by_hand_to_nothing_the_text_type_accepts_is_reported() {
        let (store, _) = store_with_events();
        lock(&store.conn)
            .execute(
                "INSERT INTO shell_init (workspace_id, content, changed_at) VALUES ('a', ' ', 1)",
                [],
            )
            .unwrap();
        assert!(matches!(
            store.shell_init(&ws("a")),
            Err(StoreError::Corrupt {
                table: "shell_init",
                ..
            })
        ));
    }
}
