// SPDX-License-Identifier: AGPL-3.0-or-later
use std::sync::{Arc, Mutex};

use super::*;

/// Records each ask; `workspaces` are the Microsoft-server ones.
struct Recorded {
    workspaces: Vec<WorkspaceId>,
    asked: Mutex<Vec<(WorkspaceId, usize)>>,
    agent_fails: bool,
    switches: bool,
}

impl Builds for Arc<Recorded> {
    fn on_microsoft_server(&self) -> Vec<WorkspaceId> {
        self.workspaces.clone()
    }

    fn switch(
        &self,
        workspace: &WorkspaceId,
        connected_windows: usize,
    ) -> impl Future<Output = Result<bool, SwitchEditorBuildError>> + Send {
        self.asked
            .lock()
            .unwrap()
            .push((workspace.clone(), connected_windows));
        if self.switches {
            return std::future::ready(Ok(true));
        }
        std::future::ready(Err(if self.agent_fails {
            SwitchEditorBuildError::Agent(puddle_agent_proto::forward::OpenError::Disconnected)
        } else {
            SwitchEditorBuildError::NotRunning
        }))
    }
}

fn name(s: &str) -> WorkspaceId {
    WorkspaceId::new(s).unwrap()
}

fn recorded(names: &[&str]) -> Arc<Recorded> {
    Arc::new(Recorded {
        workspaces: names.iter().map(|n| name(n)).collect(),
        asked: Mutex::default(),
        agent_fails: false,
        switches: false,
    })
}

#[tokio::test]
async fn a_workspace_with_a_window_is_not_asked_until_the_window_is_gone() {
    let builds = recorded(&["a"]);
    let windows = Windows::default();
    let window = windows.open(&name("a"));
    ask_once(&builds, &windows).await;
    assert!(builds.asked.lock().unwrap().is_empty());
    drop(window);
    ask_once(&builds, &windows).await;
    assert_eq!(*builds.asked.lock().unwrap(), [(name("a"), 0)]);
}

#[tokio::test]
async fn only_the_workspaces_without_a_window_are_asked_each_once_per_round() {
    let builds = recorded(&["a", "b", "c"]);
    let windows = Windows::default();
    let _on_a = windows.open(&name("a"));
    let _also_on_a = windows.open(&name("a"));
    ask_once(&builds, &windows).await;
    assert_eq!(
        *builds.asked.lock().unwrap(),
        [(name("b"), 0), (name("c"), 0)]
    );
}

#[tokio::test]
async fn a_switch_is_asked_once_per_round_and_the_round_counts_it() {
    let mut builds = recorded(&["a"]);
    Arc::get_mut(&mut builds).unwrap().switches = true;
    assert_eq!(ask_once(&builds, &Windows::default()).await, 1);
    assert_eq!(builds.asked.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_failing_agent_does_not_stop_the_round() {
    let mut builds = recorded(&["a", "b"]);
    Arc::get_mut(&mut builds).unwrap().agent_fails = true;
    ask_once(&builds, &Windows::default()).await;
    assert_eq!(builds.asked.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn a_workspace_off_the_list_is_never_asked() {
    let builds = recorded(&[]);
    ask_once(&builds, &Windows::default()).await;
    assert!(builds.asked.lock().unwrap().is_empty());
}

#[tokio::test(start_paused = true)]
async fn the_task_asks_every_period_and_not_at_once() {
    let builds = recorded(&["a"]);
    let task = spawn(builds.clone(), Windows::default());
    tokio::time::sleep(ASK_PERIOD / 2).await;
    assert_eq!(builds.asked.lock().unwrap().len(), 0);
    tokio::time::sleep(ASK_PERIOD).await;
    assert_eq!(builds.asked.lock().unwrap().len(), 1);
    tokio::time::sleep(ASK_PERIOD).await;
    assert_eq!(builds.asked.lock().unwrap().len(), 2);
    task.abort();
}
