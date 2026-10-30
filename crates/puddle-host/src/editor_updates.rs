// SPDX-License-Identifier: GPL-3.0-or-later
//! Asks each running workspace on Microsoft's editor server to switch to a newer build, at a
//! moment when no window is connected to it.
//!
//! The agent downloads and checks a new build beside the running one and tells nobody; the host
//! asks now and then, and only for a workspace the front counts no window on, because the switch
//! restarts the server and would cut a window off. Asking when nothing is staged is harmless: the
//! agent answers that it switched nothing.

use std::future::Future;
use std::time::Duration;

use puddle_front::Windows;
use puddle_types::WorkspaceName;

use crate::workspaces::SwitchEditorBuildError;

/// How often the host asks. A build the agent staged waits at most this long after the last window
/// closes; the agent itself looks for builds every day.
pub(crate) const ASK_PERIOD: Duration = Duration::from_mins(5);

/// What the asking needs from the workspaces.
pub(crate) trait Builds: Send + Sync + 'static {
    /// The running workspaces whose editor server is Microsoft's.
    fn on_microsoft_server(&self) -> Vec<WorkspaceName>;

    /// Asks `workspace`'s agent to switch to the staged build; `connected_windows` is the front's
    /// count. Whether it switched.
    fn switch(
        &self,
        workspace: &WorkspaceName,
        connected_windows: usize,
    ) -> impl Future<Output = Result<bool, SwitchEditorBuildError>> + Send;
}

/// One round: asks every Microsoft-server workspace that has no window connected.
pub(crate) async fn ask_once(builds: &impl Builds, windows: &Windows) {
    for workspace in builds.on_microsoft_server() {
        let connected = windows.connected(&workspace);
        if connected > 0 {
            continue;
        }
        match builds.switch(&workspace, connected).await {
            Ok(true) => tracing::info!(%workspace, "the editor server runs a newer build"),
            // Nothing staged, or stopped between the list and the ask: nothing to switch.
            Ok(false)
            | Err(
                SwitchEditorBuildError::NotRunning | SwitchEditorBuildError::WindowsConnected(_),
            ) => {}
            // The agent is there but failed; the next round asks again.
            Err(error @ SwitchEditorBuildError::Agent(_)) => {
                tracing::warn!(%workspace, %error, "the editor server was not switched");
            }
        }
    }
}

/// Asks every [`ASK_PERIOD`] until the returned task is aborted.
pub(crate) fn spawn<B: Builds>(builds: B, windows: Windows) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(ASK_PERIOD);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        tick.tick().await;
        loop {
            tick.tick().await;
            ask_once(&builds, &windows).await;
        }
    })
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    /// Records each ask; `workspaces` are the Microsoft-server ones.
    struct Recorded {
        workspaces: Vec<WorkspaceName>,
        asked: Mutex<Vec<(WorkspaceName, usize)>>,
        agent_fails: bool,
    }

    impl Builds for Arc<Recorded> {
        fn on_microsoft_server(&self) -> Vec<WorkspaceName> {
            self.workspaces.clone()
        }

        fn switch(
            &self,
            workspace: &WorkspaceName,
            connected_windows: usize,
        ) -> impl Future<Output = Result<bool, SwitchEditorBuildError>> + Send {
            self.asked
                .lock()
                .unwrap()
                .push((workspace.clone(), connected_windows));
            std::future::ready(Err(if self.agent_fails {
                SwitchEditorBuildError::Agent(puddle_agent_proto::forward::OpenError::Disconnected)
            } else {
                SwitchEditorBuildError::NotRunning
            }))
        }
    }

    fn name(s: &str) -> WorkspaceName {
        WorkspaceName::new(s).unwrap()
    }

    fn recorded(names: &[&str]) -> Arc<Recorded> {
        Arc::new(Recorded {
            workspaces: names.iter().map(|n| name(n)).collect(),
            asked: Mutex::default(),
            agent_fails: false,
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
}
