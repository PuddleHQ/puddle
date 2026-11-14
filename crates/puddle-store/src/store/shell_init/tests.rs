// SPDX-License-Identifier: AGPL-3.0-or-later
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

fn ws(text: &str) -> WorkspaceId {
    WorkspaceId::new(text).unwrap()
}

fn text(t: &str) -> ShellInitText {
    ShellInitText::new(t).unwrap()
}

fn store_with_events() -> (Store, Arc<Seen>) {
    let seen = Arc::new(Seen::default());
    let store = Store::open_in_memory(Arc::new(ManualClock::new(5)), Limits::default())
        .unwrap()
        .with_events(Arc::clone(&seen) as Arc<dyn EventSink>);
    for name in ["a", "b"] {
        crate::testing::own(&store, &ws(name));
    }
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
