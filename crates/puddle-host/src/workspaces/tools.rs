// SPDX-License-Identifier: GPL-3.0-or-later
//! The managed tools layer after a start: Puddle downloads the pinned mise onto the volume in the
//! background (`puddle-agent mise-fetch`). The start never waits for it and never fails because of
//! it: a failure is the problem `tools:<id>`, and the workspace runs without managed tools.

use std::time::Duration;

use puddle_compute::{ExecRequest, Runtime, Sandbox as _};
use puddle_settings::MiseMode;
use puddle_types::{Decision, Host, Problem, WorkspaceId};
use puddle_workspace::Layout;

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
        let Some(gated) = inner
            .live
            .lock()
            .await
            .get(name)
            .and_then(|l| l.gated.clone())
        else {
            return;
        };
        let Ok(dir) = Layout::new(name).and_then(|l| l.puddle_dir()) else {
            return;
        };
        let user = self.booted_user(name).await;
        let request = ExecRequest::new(
            puddle_boot::AGENT_GUEST,
            ["mise-fetch".to_owned(), format!("{dir}/mise")],
        )
        .as_user(user.name())
        .with_timeout(FETCH_TIMEOUT);
        let outcome = gated.ungated().exec(request).await;
        // A workspace stopped meanwhile says nothing about mise.
        if !inner.live.lock().await.contains_key(name) {
            return;
        }
        let reason = match outcome {
            Ok(output) if output.status.code == 0 => {
                tracing::info!(workspace = %name, result = %output.stdout_text().trim(), "mise is on the volume");
                inner.problems.clear(&key);
                return;
            }
            Ok(output) => {
                let said = output.stderr_text();
                said.lines()
                    .rev()
                    .find(|l| !l.trim().is_empty())
                    .map_or_else(
                        || format!("the download ended with exit code {}", output.status.code),
                        |l| l.trim().to_owned(),
                    )
            }
            Err(e) => e.to_string(),
        };
        tracing::warn!(workspace = %name, %reason, "mise could not be put on the volume");
        inner.problems.raise(self.mise_problem(name, &reason));
    }

    /// The problem for a failed fetch: what failed, that the workspace works without tools, and,
    /// when a rule the user made blocks the download host, which one.
    fn mise_problem(&self, name: &WorkspaceId, reason: &str) -> Problem {
        let mut detail = format!(
            "{reason}. Tools are not installed; the workspace works without them. Puddle tries \
             again at the next start."
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
        match self.inner.store.peek(name, &host)? {
            Decision::Deny { rule_id, .. } => {
                let rule = self
                    .inner
                    .store
                    .rules_every_user()
                    .into_iter()
                    .find(|r| r.id == rule_id)?;
                Some(format!(
                    "A rule you made blocks {DOWNLOAD_HOST}: deny {}. Remove it on the Network tab to allow the download.",
                    rule.pattern
                ))
            }
            Decision::SetDeny { .. } => Some(format!(
                "A rule set you switched on blocks {DOWNLOAD_HOST}; switch it off or add a rule that allows the host on the Network tab."
            )),
            _ => None,
        }
    }
}
