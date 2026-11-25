// SPDX-License-Identifier: AGPL-3.0-or-later
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use puddle_api::EventHub;
use puddle_types::EventSink;
use tokio::sync::Semaphore;

use super::*;

#[derive(Clone, Default)]
struct Fake {
    calls: Arc<Mutex<Vec<String>>>,
    running: Arc<Mutex<Vec<String>>>,
    /// Workspaces whose environment changes wait for a permit each (a credential store that
    /// does not answer until the test says so).
    held: Arc<Mutex<HashMap<String, Arc<Semaphore>>>>,
    /// How many changes of one workspace ran at once, at most.
    overlap: Arc<Mutex<(usize, usize)>>,
}

impl Fake {
    fn record(&self, call: String) {
        self.calls.lock().unwrap().push(call);
    }

    fn hold(&self, workspace: &str) -> Arc<Semaphore> {
        let gate = Arc::new(Semaphore::new(0));
        self.held
            .lock()
            .unwrap()
            .insert(workspace.to_owned(), gate.clone());
        gate
    }
}

impl Changes for Fake {
    fn decrypt_changed(&self, workspace: &WorkspaceId) -> Option<WorkspaceGit> {
        self.record(format!("decrypt {workspace}"));
        self.running
            .lock()
            .unwrap()
            .contains(&workspace.to_string())
            .then(WorkspaceGit::unconfigured)
    }

    async fn rewrite_authors(&self, workspace: WorkspaceId) {
        self.record(format!("rewrite {workspace}"));
    }

    fn all_request_rules_changed(&self) {
        self.record("all request rules".to_owned());
    }

    async fn environment_changed(&self, workspace: WorkspaceId) {
        {
            let mut overlap = self.overlap.lock().unwrap();
            overlap.0 += 1;
            overlap.1 = overlap.1.max(overlap.0);
        }
        self.record(format!("env {workspace}"));
        let gate = self.held.lock().unwrap().get(workspace.as_str()).cloned();
        if let Some(gate) = gate {
            gate.acquire().await.unwrap().forget();
        }
        self.overlap.lock().unwrap().0 -= 1;
    }

    fn running(&self) -> Vec<WorkspaceId> {
        self.running
            .lock()
            .unwrap()
            .iter()
            .map(|name| WorkspaceId::new(name).unwrap())
            .collect()
    }

    async fn shell_init_changed(&self, workspace: WorkspaceId) {
        self.record(format!("shell init {workspace}"));
    }

    fn pending_closed(&self, workspace: &WorkspaceId, id: i64, allowed: bool) {
        self.record(format!("pending {workspace} {id} {allowed}"));
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

fn id(name: &str) -> WorkspaceId {
    WorkspaceId::new(name).unwrap()
}

fn changed(name: &str) -> Event {
    Event::WorkspaceGitChanged {
        workspace: id(name),
    }
}

fn env_changed(name: &str) -> Event {
    Event::WorkspaceEnvChanged {
        workspace: id(name),
    }
}

fn follow_hub(hub: &EventHub, fake: &Fake) -> JoinHandle<()> {
    follow(hub.follow(wanted), fake.clone())
}

#[tokio::test]
async fn a_change_to_a_running_workspace_recomputes_its_decrypt_set_and_rewrites_its_authors() {
    let hub = EventHub::default();
    let fake = Fake::default();
    fake.running.lock().unwrap().push("alpha".to_owned());
    let task = follow_hub(&hub, &fake);
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
    let task = follow_hub(&hub, &fake);
    hub.emit(Event::RequestRulesChanged {});
    assert_eq!(calls_after(&fake, 1).await, ["all request rules"]);
    task.abort();
}

#[tokio::test]
async fn a_change_to_an_environment_reaches_the_workspace_it_names_or_every_running_one() {
    let hub = EventHub::default();
    let fake = Fake::default();
    fake.running
        .lock()
        .unwrap()
        .extend(["beta".to_owned(), "gamma".to_owned()]);
    let task = follow_hub(&hub, &fake);
    hub.emit(env_changed("alpha"));
    hub.emit(Event::IdentitiesChanged {});
    assert_eq!(calls_after(&fake, 1).await, ["env alpha"]);
    hub.emit(Event::GlobalEnvChanged {});
    assert_eq!(
        calls_after(&fake, 3).await,
        ["env alpha", "env beta", "env gamma"]
    );
    task.abort();
}

#[tokio::test]
async fn a_change_to_a_shell_init_reaches_the_workspace_it_names() {
    let hub = EventHub::default();
    let fake = Fake::default();
    let task = follow_hub(&hub, &fake);
    hub.emit(Event::WorkspaceShellInitChanged {
        workspace: id("alpha"),
    });
    hub.emit(Event::WorkspaceShellInitRestored {
        workspace: id("beta"),
    });
    assert_eq!(calls_after(&fake, 1).await, ["shell init alpha"]);
    task.abort();
}

#[tokio::test]
async fn a_closed_pending_request_reaches_the_workspace_it_names_saying_whether_it_was_allowed() {
    let hub = EventHub::default();
    let fake = Fake::default();
    let task = follow_hub(&hub, &fake);
    for (n, state) in [
        (1, PendingEnd::Allowed),
        (2, PendingEnd::Denied),
        (3, PendingEnd::Expired),
    ] {
        hub.emit(Event::PendingClosed {
            workspace: id("alpha"),
            id: n,
            state,
            rule_id: None,
        });
    }
    assert_eq!(
        calls_after(&fake, 3).await,
        [
            "pending alpha 1 true",
            "pending alpha 2 false",
            "pending alpha 3 false"
        ]
    );
    task.abort();
}

#[tokio::test]
async fn more_changes_than_the_event_stream_holds_are_all_applied() {
    // A stream of this hub holds one event; the feed of the changes holds them all.
    let hub = EventHub::new(1);
    let fake = Fake::default();
    let names: Vec<String> = (0..50).map(|n| format!("w{n}")).collect();
    fake.running.lock().unwrap().extend(names.iter().cloned());
    let task = follow_hub(&hub, &fake);
    for name in &names {
        hub.emit(changed(name));
        hub.emit(Event::RulesChanged {});
    }
    let calls = calls_after(&fake, 100).await;
    for name in &names {
        assert!(
            calls.contains(&format!("rewrite {name}")),
            "{name}: {calls:?}"
        );
    }
    task.abort();
}

#[tokio::test]
async fn a_workspace_whose_change_waits_holds_up_no_other_workspace() {
    let hub = EventHub::default();
    let fake = Fake::default();
    let alpha = fake.hold("alpha");
    let task = follow_hub(&hub, &fake);
    hub.emit(env_changed("alpha"));
    hub.emit(env_changed("beta"));
    hub.emit(Event::WorkspaceShellInitChanged {
        workspace: id("beta"),
    });
    assert_eq!(
        calls_after(&fake, 3).await,
        ["env alpha", "env beta", "shell init beta"]
    );
    alpha.add_permits(1);
    task.abort();
}

#[tokio::test]
async fn changes_to_one_workspace_run_one_at_a_time_and_one_that_already_waits_is_not_queued_again()
{
    let hub = EventHub::default();
    let fake = Fake::default();
    let alpha = fake.hold("alpha");
    let task = follow_hub(&hub, &fake);
    hub.emit(env_changed("alpha"));
    assert_eq!(calls_after(&fake, 1).await, ["env alpha"]);
    // Three more arrive while the first runs: they wait, as one, behind it, and so does the
    // shell init change.
    for _ in 0..3 {
        hub.emit(env_changed("alpha"));
    }
    hub.emit(Event::WorkspaceShellInitChanged {
        workspace: id("alpha"),
    });
    hub.emit(Event::RulesChanged {});
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(fake.calls.lock().unwrap().len(), 1, "a change did not wait");
    alpha.add_permits(2);
    assert_eq!(
        calls_after(&fake, 3).await,
        ["env alpha", "env alpha", "shell init alpha"]
    );
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(fake.calls.lock().unwrap().len(), 3, "a change ran twice");
    assert_eq!(fake.overlap.lock().unwrap().1, 1, "two changes ran at once");
    // The queue's task has ended, and the next change starts it again.
    hub.emit(env_changed("alpha"));
    alpha.add_permits(1);
    assert_eq!(calls_after(&fake, 4).await.len(), 4);
    task.abort();
}

#[tokio::test]
async fn the_task_ends_when_the_hub_is_gone() {
    let hub = EventHub::default();
    let task = follow_hub(&hub, &Fake::default());
    drop(hub);
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();
}
