// SPDX-License-Identifier: GPL-3.0-or-later
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

impl Seen {
    fn take(&self) -> Vec<Event> {
        std::mem::take(&mut self.0.lock().unwrap())
    }
}

fn ws(text: &str) -> WorkspaceId {
    WorkspaceId::new(text).unwrap()
}

fn store() -> (Arc<ManualClock>, Store, Arc<Seen>) {
    let clock = Arc::new(ManualClock::new(1_000));
    let seen = Arc::new(Seen::default());
    let store = Store::open_in_memory(clock.clone(), Limits::default())
        .unwrap()
        .with_events(Arc::clone(&seen) as Arc<dyn EventSink>);
    (clock, store, seen)
}

fn changed_a() -> Vec<Event> {
    vec![Event::WorkspaceGitChanged { workspace: ws("a") }]
}

fn keys(list: &[&str]) -> Vec<String> {
    list.iter().map(|k| (*k).to_owned()).collect()
}

#[test]
fn no_host_has_the_agent_until_the_user_turns_it_on() {
    let (_, store, _) = store();
    assert_eq!(store.ssh_agent_hosts(&ws("a")).unwrap(), []);
}

#[test]
fn turning_it_on_keeps_the_host_and_its_keys_sorted_once_each_for_that_workspace_only() {
    let (clock, store, seen) = store();
    let on = store
        .set_ssh_agent_host(
            &ws("a"),
            "build.example.com",
            Some(&keys(&["SHA256:b", "SHA256:a", "SHA256:b"])),
        )
        .unwrap();
    let want = SshAgentHost {
        host: "build.example.com".into(),
        keys: Some(keys(&["SHA256:a", "SHA256:b"])),
        changed_at: 1_000,
    };
    assert_eq!(on, want);
    assert_eq!(store.ssh_agent_hosts(&ws("a")).unwrap(), vec![want]);
    assert_eq!(store.ssh_agent_hosts(&ws("b")).unwrap(), []);
    assert_eq!(seen.take(), changed_a());

    clock.advance(500);
    let every = store
        .set_ssh_agent_host(&ws("a"), "build.example.com", None)
        .unwrap();
    assert_eq!((every.keys, every.changed_at), (None, 1_500));
    assert_eq!(seen.take(), changed_a());
}

#[test]
fn saving_what_is_already_saved_changes_nothing_and_tells_no_one() {
    let (clock, store, seen) = store();
    store
        .set_ssh_agent_host(&ws("a"), "h.example", Some(&keys(&["SHA256:a"])))
        .unwrap();
    seen.take();
    clock.advance(500);
    let again = store
        .set_ssh_agent_host(&ws("a"), "h.example", Some(&keys(&["SHA256:a"])))
        .unwrap();
    assert_eq!(again.changed_at, 1_000);
    assert_eq!(seen.take(), []);
}

#[test]
fn turning_it_off_removes_the_host_and_says_whether_it_was_on() {
    let (_, store, seen) = store();
    store
        .set_ssh_agent_host(&ws("a"), "h.example", None)
        .unwrap();
    store
        .set_ssh_agent_host(&ws("a"), "i.example", None)
        .unwrap();
    seen.take();
    assert!(store.remove_ssh_agent_host(&ws("a"), "h.example").unwrap());
    assert_eq!(seen.take(), changed_a());
    assert!(!store.remove_ssh_agent_host(&ws("a"), "h.example").unwrap());
    assert_eq!(seen.take(), []);
    let left: Vec<String> = store
        .ssh_agent_hosts(&ws("a"))
        .unwrap()
        .into_iter()
        .map(|h| h.host)
        .collect();
    assert_eq!(left, ["i.example"]);
}

#[test]
fn a_workspace_may_have_the_agent_on_for_at_most_the_limit_of_hosts() {
    let (_, store, _) = store();
    for n in 0..MAX_SSH_AGENT_HOSTS {
        store
            .set_ssh_agent_host(&ws("a"), &format!("h{n}.example"), None)
            .unwrap();
    }
    let err = store
        .set_ssh_agent_host(&ws("a"), "one-more.example", None)
        .unwrap_err();
    assert!(matches!(
        err,
        StoreError::TooManySshAgentHosts(MAX_SSH_AGENT_HOSTS)
    ));
    // A host already on may still change its keys, and another workspace has its own limit.
    store
        .set_ssh_agent_host(&ws("a"), "h0.example", Some(&keys(&["SHA256:a"])))
        .unwrap();
    store
        .set_ssh_agent_host(&ws("b"), "h.example", None)
        .unwrap();
}

#[test]
fn deleting_the_workspace_drops_its_hosts() {
    let (_, store, _) = store();
    store
        .set_ssh_agent_host(&ws("a"), "h.example", None)
        .unwrap();
    store
        .set_ssh_agent_host(&ws("b"), "h.example", None)
        .unwrap();
    store.delete_workspace(&ws("a")).unwrap();
    assert_eq!(store.ssh_agent_hosts(&ws("a")).unwrap(), []);
    assert_eq!(store.ssh_agent_hosts(&ws("b")).unwrap().len(), 1);
}
