// SPDX-License-Identifier: AGPL-3.0-or-later
use std::sync::{Arc, Mutex};
use std::time::Duration;

use puddle_api::EventHub;
use puddle_types::EventSink;

use super::*;

#[derive(Clone, Default)]
struct Fake {
    calls: Arc<Mutex<Vec<String>>>,
    running: Arc<Mutex<Vec<String>>>,
}

impl Changes for Fake {
    fn decrypt_changed(&self, workspace: &WorkspaceId) -> Option<WorkspaceGit> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("decrypt {workspace}"));
        self.running
            .lock()
            .unwrap()
            .contains(&workspace.to_string())
            .then(WorkspaceGit::unconfigured)
    }

    async fn rewrite_authors(&self, workspace: WorkspaceId) {
        self.calls
            .lock()
            .unwrap()
            .push(format!("rewrite {workspace}"));
    }

    async fn all_git_changed(&self) {
        self.calls.lock().unwrap().push("all".to_owned());
    }

    fn all_request_rules_changed(&self) {
        self.calls
            .lock()
            .unwrap()
            .push("all request rules".to_owned());
    }

    async fn environment_changed(&self, workspace: WorkspaceId) {
        self.calls.lock().unwrap().push(format!("env {workspace}"));
    }

    async fn all_environments_changed(&self) {
        self.calls.lock().unwrap().push("all env".to_owned());
    }

    async fn shell_init_changed(&self, workspace: WorkspaceId) {
        self.calls
            .lock()
            .unwrap()
            .push(format!("shell init {workspace}"));
    }

    async fn all_shell_inits_changed(&self) {
        self.calls.lock().unwrap().push("all shell init".to_owned());
    }
}

async fn calls_after(fake: &Fake, want: usize) -> Vec<String> {
    puddle_test_kit::eventually(
        &format!("{want} calls recorded"),
        Duration::from_secs(5),
        || fake.calls.lock().unwrap().len() >= want,
    )
    .await;
    let mut calls = fake.calls.lock().unwrap().clone();
    calls.sort();
    calls
}

fn changed(name: &str) -> Event {
    Event::WorkspaceGitChanged {
        workspace: WorkspaceId::new(name).unwrap(),
    }
}

#[tokio::test]
async fn a_change_to_a_running_workspace_recomputes_its_decrypt_set_and_rewrites_its_authors() {
    let hub = EventHub::default();
    let fake = Fake::default();
    fake.running.lock().unwrap().push("alpha".to_owned());
    let task = follow(hub.subscribe(), fake.clone());
    hub.emit(Event::RulesChanged {});
    hub.emit(changed("alpha"));
    hub.emit(changed("beta"));
    // beta does not run: its set is looked at (it has none) and nothing is rewritten.
    assert_eq!(
        calls_after(&fake, 3).await,
        ["decrypt alpha", "decrypt beta", "rewrite alpha"]
    );
    task.abort();
}

#[tokio::test]
async fn a_change_to_the_request_rules_reaches_every_running_workspace() {
    let hub = EventHub::default();
    let fake = Fake::default();
    let task = follow(hub.subscribe(), fake.clone());
    hub.emit(Event::RequestRulesChanged {});
    assert_eq!(calls_after(&fake, 1).await, ["all request rules"]);
    task.abort();
}

#[tokio::test]
async fn a_change_to_an_environment_reaches_the_workspace_it_names_or_every_one() {
    let hub = EventHub::default();
    let fake = Fake::default();
    let task = follow(hub.subscribe(), fake.clone());
    hub.emit(Event::WorkspaceEnvChanged {
        workspace: WorkspaceId::new("alpha").unwrap(),
    });
    hub.emit(Event::GlobalEnvChanged {});
    hub.emit(Event::IdentitiesChanged {});
    assert_eq!(calls_after(&fake, 2).await, ["all env", "env alpha"]);
    // Git changes are not environment changes and the other way round.
    hub.emit(changed("beta"));
    assert_eq!(
        calls_after(&fake, 3).await,
        ["all env", "decrypt beta", "env alpha"]
    );
    task.abort();
}

#[tokio::test]
async fn a_change_to_a_shell_init_reaches_the_workspace_it_names() {
    let hub = EventHub::default();
    let fake = Fake::default();
    let task = follow(hub.subscribe(), fake.clone());
    hub.emit(Event::WorkspaceShellInitChanged {
        workspace: WorkspaceId::new("alpha").unwrap(),
    });
    hub.emit(Event::WorkspaceShellInitRestored {
        workspace: WorkspaceId::new("beta").unwrap(),
    });
    assert_eq!(calls_after(&fake, 1).await, ["shell init alpha"]);
    task.abort();
}

#[tokio::test]
async fn missed_events_make_every_running_workspace_be_looked_at_again() {
    // A hub that holds one event: sending three before the task runs makes it lag.
    let hub = EventHub::new(1);
    let fake = Fake::default();
    let task = follow(hub.subscribe(), fake.clone());
    for name in ["a", "b", "c"] {
        hub.emit(changed(name));
    }
    let calls = calls_after(&fake, 2).await;
    assert!(calls.contains(&"all".to_owned()), "{calls:?}");
    assert!(calls.contains(&"all env".to_owned()), "{calls:?}");
    let calls = calls_after(&fake, 3).await;
    assert!(calls.contains(&"all shell init".to_owned()), "{calls:?}");
    task.abort();
}

#[tokio::test]
async fn the_task_ends_when_the_hub_is_gone() {
    let hub = EventHub::default();
    let task = follow(hub.subscribe(), Fake::default());
    drop(hub);
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();
}
