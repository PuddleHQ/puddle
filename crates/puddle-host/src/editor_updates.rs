// SPDX-License-Identifier: AGPL-3.0-or-later
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
use puddle_types::WorkspaceId;

use crate::workspaces::SwitchEditorBuildError;

/// How often the host asks. A build the agent staged waits at most this long after the last window
/// closes; the agent itself looks for builds every day.
pub(crate) const ASK_PERIOD: Duration = Duration::from_mins(5);

/// What the asking needs from the workspaces.
pub(crate) trait Builds: Send + Sync + 'static {
    /// The running workspaces whose editor server is Microsoft's.
    fn on_microsoft_server(&self) -> Vec<WorkspaceId>;

    /// Asks `workspace`'s agent to switch to the staged build; `connected_windows` is the front's
    /// count. Whether it switched.
    fn switch(
        &self,
        workspace: &WorkspaceId,
        connected_windows: usize,
    ) -> impl Future<Output = Result<bool, SwitchEditorBuildError>> + Send;
}

/// One round: asks every Microsoft-server workspace that has no window connected, and says how
/// many it asked.
pub(crate) async fn ask_once(builds: &impl Builds, windows: &Windows) -> usize {
    let mut asked = 0;
    for workspace in builds.on_microsoft_server() {
        let connected = windows.connected(&workspace);
        if connected > 0 {
            continue;
        }
        asked += 1;
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
    asked
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
mod tests;
