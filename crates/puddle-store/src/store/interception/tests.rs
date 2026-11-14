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

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_owned()).collect()
}

fn store() -> (Store, Arc<Seen>) {
    let seen = Arc::new(Seen::default());
    let store = Store::open_in_memory(Arc::new(ManualClock::new(1)), Limits::default())
        .unwrap()
        .with_events(Arc::clone(&seen) as Arc<dyn EventSink>);
    for name in ["a", "b", "off", "on", "other", "own", "shop"] {
        crate::testing::own(&store, &ws(name));
    }
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
fn a_workspace_follows_the_global_setting_until_it_picks_a_mode() {
    let (store, seen) = store();
    assert_eq!(
        store.workspace_interception(&ws("a")).unwrap(),
        WorkspaceInterception::default()
    );
    store
        .set_workspace_mode(&ws("a"), Some(InterceptionMode::All))
        .unwrap();
    store
        .set_workspace_mode(&ws("a"), Some(InterceptionMode::All))
        .unwrap();
    assert_eq!(
        store.workspace_interception(&ws("a")).unwrap().mode,
        Some(InterceptionMode::All)
    );
    assert_eq!(store.workspace_interception(&ws("b")).unwrap().mode, None);
    assert_eq!(changes(&seen), 1, "an unchanged value is not a change");
    store.set_workspace_mode(&ws("a"), None).unwrap();
    assert_eq!(store.workspace_interception(&ws("a")).unwrap().mode, None);
    assert_eq!(changes(&seen), 2);
}

#[test]
fn the_global_setting_is_an_unchosen_known_only_with_learning_on_until_saved() {
    let (store, seen) = store();
    let fresh = store.interception_settings().unwrap();
    assert_eq!(fresh, InterceptionSettings::default());
    assert_eq!(fresh.effective_mode(), InterceptionMode::KnownOnly);
    assert!(fresh.learn_from_rejections && fresh.mode.is_none() && !fresh.notice_seen);

    store.set_workspace_mode(&ws("own"), None).unwrap();
    let before = changes(&seen);
    let chosen = InterceptionSettings {
        mode: Some(InterceptionMode::AllExceptCurated),
        ..fresh
    };
    store.set_interception_settings(chosen).unwrap();
    assert_eq!(store.interception_settings().unwrap(), chosen);
    assert_eq!(
        store.interception_settings().unwrap().effective_mode(),
        InterceptionMode::AllExceptCurated
    );
    assert_eq!(changes(&seen) - before, 1, "every workspace is told");

    let seen_notice = InterceptionSettings {
        notice_seen: true,
        ..chosen
    };
    store.set_interception_settings(seen_notice).unwrap();
    assert_eq!(
        changes(&seen) - before,
        1,
        "the notice flag changes nothing a workspace decrypts"
    );
    store
        .set_interception_settings(InterceptionSettings {
            learn_from_rejections: false,
            ..seen_notice
        })
        .unwrap();
    assert_eq!(changes(&seen) - before, 2);
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
fn the_global_list_tells_every_workspace_that_has_settings() {
    let (store, seen) = store();
    store
        .set_workspace_mode(&ws("on"), Some(InterceptionMode::All))
        .unwrap();
    store
        .set_workspace_mode(&ws("off"), Some(InterceptionMode::KnownOnly))
        .unwrap();
    let before = changes(&seen);
    store
        .set_global_never_decrypt(&strings(&["bank.example"]))
        .unwrap();
    assert_eq!(store.global_never_decrypt().unwrap(), ["bank.example"]);
    assert_eq!(changes(&seen) - before, 2);
    store.set_global_never_decrypt(&[]).unwrap();
    assert_eq!(store.global_never_decrypt().unwrap(), strings(&[]));
}

#[test]
fn the_learned_limits_are_twelve_hours_and_1024_until_saved_and_tell_the_workspaces() {
    let (store, seen) = store();
    let built_in = LearnedLimits::default();
    assert_eq!((built_in.ttl_secs, built_in.cap), (43_200, 1024));
    assert_eq!(store.learned_limits().unwrap(), built_in);
    store
        .set_workspace_mode(&ws("on"), Some(InterceptionMode::All))
        .unwrap();
    let before = changes(&seen);
    let mine = LearnedLimits {
        ttl_secs: 600,
        cap: 8,
    };
    store.set_learned_limits(mine).unwrap();
    assert_eq!(store.learned_limits().unwrap(), mine);
    assert_eq!(changes(&seen) - before, 1, "the workspace the store knows");
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
    store
        .set_workspace_mode(&ws("a"), Some(InterceptionMode::All))
        .unwrap();
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

#[test]
fn every_never_decrypt_name_is_listed_with_its_workspace() {
    let (store, _) = store();
    store
        .set_global_never_decrypt(&strings(&["g.example"]))
        .unwrap();
    store
        .set_never_decrypt(&ws("shop"), &strings(&["*.bank.example"]))
        .unwrap();
    store
        .set_never_decrypt(&ws("other"), &strings(&["o.example"]))
        .unwrap();
    let named: Vec<String> = store
        .never_decrypt_names()
        .unwrap()
        .into_iter()
        .map(|(w, p)| format!("{}:{p}", w.unwrap_or_default()))
        .collect();
    assert_eq!(
        named,
        [":g.example", "other:o.example", "shop:*.bank.example"]
    );
}
