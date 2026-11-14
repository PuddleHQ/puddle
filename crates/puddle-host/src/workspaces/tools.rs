// SPDX-License-Identifier: GPL-3.0-or-later
//! The managed tools layer after a start: Puddle downloads the pinned mise onto the volume in the
//! background (`puddle-agent mise-fetch`). The start never waits for it and never fails because of
//! it: a failure is the problem `tools:<id>`, and the workspace runs without managed tools.

use std::time::Duration;

use puddle_compute::{ComputeError, ExecOutput, ExecRequest, Runtime, Sandbox as _};
use puddle_settings::MiseMode;
use puddle_store::UserId;
use puddle_types::{Decision, Host, Problem, RuleId, WorkspaceId};
use puddle_workspace::{PUDDLE_DIR, WORKSPACES_ROOT};

use super::{HostWorkspaces, problem_key};

/// The host the download comes from: the one an own deny rule would block it on.
const DOWNLOAD_HOST: &str = "mise.jdx.dev";

/// How long the fetch may take: a stalled transfer ends sooner (the downloader's own limit), this
/// only bounds a download that crawls.
const FETCH_TIMEOUT: Duration = Duration::from_mins(30);

/// The key of the problem about one workspace's tools.
pub(super) fn tools_key(name: &WorkspaceId) -> String {
    problem_key("tools", name)
}

impl<R: Runtime + Clone> HostWorkspaces<R> {
    /// After a start has settled: fetches mise on a task of the service, when the layer is on for
    /// the workspace. Returns at once.
    pub(super) fn fetch_mise_later(&self, name: &WorkspaceId) {
        let (this, name) = (self.clone(), name.clone());
        self.spawn(async move { this.fetch_mise(&name).await });
    }

    async fn fetch_mise(&self, name: &WorkspaceId) {
        let inner = &self.inner;
        let key = tools_key(name);
        match crate::settings_read::mise(inner.settings.as_ref(), name) {
            Ok(MiseMode::Off) => {
                inner.problems.clear(&key);
                return;
            }
            Ok(MiseMode::On { .. }) => {}
            Err(reason) => {
                tracing::warn!(workspace = %name, %reason, "mise was not fetched: the settings are unreadable");
                return;
            }
        }
        // `None`: the workspace was not running before or after the fetch, which says nothing
        // about mise.
        if let Some(outcome) = self.run_mise_fetch(name).await {
            self.record_mise_outcome(name, outcome);
        }
    }

    /// Clears the problem after a good fetch, raises it after a failed one.
    fn record_mise_outcome(&self, name: &WorkspaceId, outcome: Result<ExecOutput, ComputeError>) {
        let problems = &self.inner.problems;
        match outcome {
            Ok(output) if output.status.code == 0 => {
                tracing::info!(workspace = %name, result = %output.stdout_text().trim(), "mise is on the volume");
                problems.clear(&tools_key(name));
            }
            failed => {
                let reason = failure_reason(failed);
                tracing::warn!(workspace = %name, %reason, "mise could not be put on the volume");
                problems.raise(self.mise_problem(name, &reason));
            }
        }
    }

    /// Runs `puddle-agent mise-fetch` in the guest as the session user. `None` when the workspace
    /// is not running, at the start or when the command ended.
    async fn run_mise_fetch(&self, name: &WorkspaceId) -> Option<Result<ExecOutput, ComputeError>> {
        let inner = &self.inner;
        let gated = inner.live.lock().await.get(name)?.gated.clone()?;
        let dir = format!("{WORKSPACES_ROOT}/{name}/{PUDDLE_DIR}/mise");
        let user = self.booted_user(name).await;
        let request = ExecRequest::new(puddle_boot::AGENT_GUEST, ["mise-fetch".to_owned(), dir])
            .as_user(user.name())
            .with_timeout(FETCH_TIMEOUT);
        let outcome = gated.ungated().exec(request).await;
        // A stop keeps the entry but takes its sandbox away.
        let running = inner
            .live
            .lock()
            .await
            .get(name)
            .is_some_and(|l| l.gated.is_some());
        running.then_some(outcome)
    }

    /// The problem for a failed fetch: what failed, that the workspace works without tools, and,
    /// when a rule the user made blocks the download host, which one.
    fn mise_problem(&self, name: &WorkspaceId, reason: &str) -> Problem {
        let mut detail = format!(
            "Setting up mise failed: {reason}. Tools are not installed; the workspace works without them. Puddle \
             tries again at the next start."
        );
        if let Some(blocker) = self.download_blocker(name) {
            detail.push(' ');
            detail.push_str(&blocker);
        }
        Problem::new(
            tools_key(name),
            format!("Puddle could not set up mise in {}", self.label(name)),
            detail,
        )
        .about(name.clone())
    }

    /// Which rule of the user's blocks the download host for `name`, in words.
    fn download_blocker(&self, name: &WorkspaceId) -> Option<String> {
        let host = Host::parse_normalised(DOWNLOAD_HOST).ok()?;
        let store = &self.inner.store;
        let owner = store
            .workspace_owner(name)
            .ok()
            .flatten()
            .unwrap_or(UserId::OWNER);
        let decision = store.as_user(owner).peek(name, &host)?;
        blocker_words(decision, |id| {
            self.inner
                .store
                .rules(owner)
                .into_iter()
                .find(|r| r.id == id)
                .map(|r| r.pattern.to_string())
        })
    }
}

/// What a failed fetch says: the last line the command wrote to standard error (the agent's
/// own message), else its exit code, else why the command could not run.
fn failure_reason(outcome: Result<ExecOutput, ComputeError>) -> String {
    match outcome {
        Ok(output) => output
            .stderr_text()
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .map_or_else(
                || format!("the download ended with exit code {}", output.status.code),
                |l| l.trim().to_owned(),
            ),
        Err(e) => e.to_string(),
    }
}

/// The sentence about a rule that blocks the download host, when `decision` is a deny. `pattern`
/// reads the pattern of a rule by its id.
fn blocker_words(decision: Decision, pattern: impl Fn(RuleId) -> Option<String>) -> Option<String> {
    match decision {
        Decision::Deny { rule_id, .. } => Some(format!(
            "A rule you made blocks {DOWNLOAD_HOST}: deny {}. Remove it on the Network tab to allow the download.",
            pattern(rule_id)?
        )),
        Decision::SetDeny { .. } => Some(format!(
            "A rule set you switched on blocks {DOWNLOAD_HOST}; switch it off or add a rule that allows the host on the Network tab."
        )),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
