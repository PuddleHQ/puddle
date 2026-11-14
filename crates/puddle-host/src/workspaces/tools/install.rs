// SPDX-License-Identifier: GPL-3.0-or-later
//! The install of a workspace's managed tools, after mise is on the volume: `puddle-agent
//! mise-install` in the guest as the session user, followed by a report every two seconds
//! (`--missing`) while it runs. The guest's report is hostile input: [`parse_report`] takes known
//! records only, bounded, with an end marker, and refuses the whole report otherwise.
//!
//! What the user sees is the event [`Event::WorkspaceTools`] and, when tools are still missing,
//! the problem `tools:<id>`. A failed install names the hosts it waited on: the workspace's
//! pending requests that were opened, or seen again, while it ran. When the user allows one of
//! them, the install runs once more ([`super::ToolRuns::closed`]).

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use puddle_compute::{ComputeError, ExecOutput, ExecRequest, Runtime, Sandbox as _};
use puddle_types::{Event, Problem, ToolsState, WorkspaceId};
use puddle_workspace::WORKSPACES_ROOT;

use super::{HostWorkspaces, tools_key};

/// How long the install may take: a JDK and two runtimes through a slow proxy take minutes.
const INSTALL_TIMEOUT: Duration = Duration::from_mins(60);

/// How long one report of what is missing may take.
const REPORT_TIMEOUT: Duration = Duration::from_mins(2);

/// How often the host asks what is still missing while an install runs.
const PROGRESS_EVERY: Duration = Duration::from_secs(2);

/// The most records a report may hold, and the longest tool name: a guest that sends more is not
/// believed.
const MAX_RECORDS: usize = 500;
const MAX_TOOL_CHARS: usize = 200;

/// How long the trust step for a newly installed JDK may take (it caps its own work at about
/// 20 s per JDK).
const JAVA_TRUST_TIMEOUT: Duration = Duration::from_mins(5);

/// What `mise-install` reported.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Report {
    pub(super) missing: BTreeSet<String>,
    pub(super) installed: BTreeSet<String>,
    pub(super) failed: BTreeSet<String>,
}

/// Reads a report of `mise-install` (see `puddle_agent::mise_install`). `None` for anything that
/// is not exactly that: an unknown record, a tool name that is empty, too long or holds a space
/// or a control character, too many records, or no `end` as the last line.
pub(super) fn parse_report(stdout: &str) -> Option<Report> {
    let mut report = Report::default();
    let mut lines = stdout.lines();
    let mut ended = false;
    for (n, line) in lines.by_ref().enumerate() {
        if n > MAX_RECORDS {
            return None;
        }
        if line == "end" {
            ended = true;
            break;
        }
        let (word, tool) = line.split_once(' ')?;
        let good = !tool.is_empty()
            && tool.chars().count() <= MAX_TOOL_CHARS
            && !tool.chars().any(|c| c.is_whitespace() || c.is_control());
        if !good {
            return None;
        }
        let into = match word {
            "missing" => &mut report.missing,
            "installed" => &mut report.installed,
            "failed" => &mut report.failed,
            _ => return None,
        };
        into.insert(tool.to_owned());
    }
    (ended && lines.all(|l| l.trim().is_empty())).then_some(report)
}

/// What an install's outcome says: the report (empty when unreadable), and why it failed, `None`
/// when it did not. The reason is the last line the agent wrote to standard error (mise's own
/// error line), else what is wrong with the outcome.
fn read_install(outcome: Result<ExecOutput, ComputeError>) -> (Report, Option<String>) {
    let out = match outcome {
        Ok(out) => out,
        Err(e) => return (Report::default(), Some(e.to_string())),
    };
    let Some(report) = parse_report(&out.stdout_text()) else {
        return (
            Report::default(),
            Some(format!(
                "the guest's report could not be read (exit code {})",
                out.status.code
            )),
        );
    };
    if out.status.code == 0 && report.failed.is_empty() {
        return (report, None);
    }
    let reason = out
        .stderr_text()
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map_or_else(
            || format!("the install ended with exit code {}", out.status.code),
            str::to_owned,
        );
    (report, Some(reason))
}

/// Whether one of `tools` is a JDK, whose trust store then needs Puddle's CAs.
fn has_java(tools: &BTreeSet<String>) -> bool {
    tools.iter().any(|t| t.starts_with("java@"))
}

/// A count for the event, which a report's bound keeps small.
fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

impl<R: Runtime + Clone> HostWorkspaces<R> {
    /// Installs what the workspace's mise files ask for, reporting progress, and clears or raises
    /// the problem. The caller holds the workspace's tool-run lock and has put mise on the volume.
    pub(super) async fn install_tools(&self, name: &WorkspaceId) {
        let pending_before = self.open_pending_attempts(name);
        let Some(before) = self.missing_tools(name).await else {
            return;
        };
        if before.missing.is_empty() {
            self.tools_ready(name);
            return;
        }
        let total = before.missing.len();
        self.tools_event(
            name,
            ToolsState::Installing {
                tools: before.missing.iter().cloned().collect(),
                done: 0,
                total: count(total),
            },
        );
        let Some(outcome) = self.run_install(name, total).await else {
            return;
        };
        let (report, reason) = read_install(outcome);
        if has_java(&report.installed) {
            self.trust_new_jdk(name).await;
        }
        let Some(reason) = reason else {
            self.tools_ready(name);
            return;
        };
        let waiting = self.waited_on(name, &pending_before);
        let hosts: Vec<String> = waiting
            .values()
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        tracing::warn!(workspace = %name, %reason, waiting_on = ?hosts, "tools were not installed");
        self.inner
            .tools
            .wait_on(name, waiting.keys().copied().collect());
        self.inner
            .problems
            .raise(self.install_problem(name, &report.failed, &reason, &hosts));
        self.tools_event(
            name,
            ToolsState::Failed {
                tools: report.failed.into_iter().collect(),
                waiting_on: hosts,
            },
        );
    }

    /// Every tool is in place: the problem goes, nothing is waited on.
    fn tools_ready(&self, name: &WorkspaceId) {
        self.inner.tools.wait_on(name, BTreeSet::new());
        self.inner.problems.clear(&tools_key(name));
        self.tools_event(name, ToolsState::Ready);
    }

    /// Runs the install, and while it runs reports progress against the `total` tools that were
    /// missing. `None` when the workspace is not running at the start or the end.
    async fn run_install(
        &self,
        name: &WorkspaceId,
        total: usize,
    ) -> Option<Result<ExecOutput, ComputeError>> {
        let install = self.mise_install(name, false, INSTALL_TIMEOUT);
        let progress = async {
            let mut last = 0;
            loop {
                tokio::time::sleep(PROGRESS_EVERY).await;
                let Some(now) = self.missing_tools(name).await else {
                    continue;
                };
                let done = total.saturating_sub(now.missing.len());
                if done != last {
                    last = done;
                    self.tools_event(
                        name,
                        ToolsState::Installing {
                            tools: now.missing.into_iter().collect(),
                            done: count(done),
                            total: count(total),
                        },
                    );
                }
            }
        };
        tokio::select! {
            outcome = install => outcome,
            () = progress => None,
        }
    }

    /// What is missing now, from `mise-install --missing`. `None` when the workspace is not
    /// running or the report could not be read (which is logged).
    async fn missing_tools(&self, name: &WorkspaceId) -> Option<Report> {
        let outcome = self.mise_install(name, true, REPORT_TIMEOUT).await?;
        match outcome {
            Ok(out) => {
                let report = parse_report(&out.stdout_text());
                if report.is_none() {
                    tracing::warn!(workspace = %name, code = out.status.code, stderr = %out.stderr_text().trim(), "the guest's report of missing tools was not readable");
                }
                report
            }
            Err(e) => {
                tracing::warn!(workspace = %name, error = %e, "the guest was not asked which tools are missing");
                None
            }
        }
    }

    /// Runs `puddle-agent mise-install <workspace-dir>` (with `--missing` when `report_only`) as
    /// the session user. `None` when the workspace is not running, at the start or at the end.
    async fn mise_install(
        &self,
        name: &WorkspaceId,
        report_only: bool,
        timeout: Duration,
    ) -> Option<Result<ExecOutput, ComputeError>> {
        let inner = &self.inner;
        let gated = inner.live.lock().await.get(name)?.gated.clone()?;
        let mut args = vec![
            "mise-install".to_owned(),
            format!("{WORKSPACES_ROOT}/{name}"),
        ];
        if report_only {
            args.push("--missing".to_owned());
        }
        let user = self.booted_user(name).await;
        let request = ExecRequest::new(puddle_boot::AGENT_GUEST, args)
            .as_user(user.name())
            .with_timeout(timeout);
        let outcome = gated.ungated().exec(request).await;
        let running = inner
            .live
            .lock()
            .await
            .get(name)
            .is_some_and(|l| l.gated.is_some());
        running.then_some(outcome)
    }

    /// Imports Puddle's CAs into the JDK mise just installed: the boot ran the same step before
    /// the JDK existed.
    async fn trust_new_jdk(&self, name: &WorkspaceId) {
        let gated = match self.inner.live.lock().await.get(name) {
            Some(live) => live.gated.clone(),
            None => None,
        };
        let Some(gated) = gated else { return };
        let request =
            ExecRequest::new("sh", [puddle_certs::JAVA_STEP_PATH]).with_timeout(JAVA_TRUST_TIMEOUT);
        match gated.ungated().exec(request).await {
            Ok(out) if out.status.code == 0 => {
                tracing::info!(workspace = %name, said = %out.stdout_text().trim(), "the new JDK trusts Puddle's CAs");
            }
            Ok(out) => {
                tracing::warn!(workspace = %name, code = out.status.code, said = %out.stdout_text().trim(), "the new JDK's trust store was not updated");
            }
            Err(e) => {
                tracing::warn!(workspace = %name, error = %e, "the new JDK's trust store was not updated");
            }
        }
    }

    /// The workspace's open pending requests, with how often each was asked for.
    fn open_pending_attempts(&self, name: &WorkspaceId) -> BTreeMap<i64, u64> {
        self.inner
            .store
            .open_pending_every_user(&self.inner.all_users, Some(name))
            .unwrap_or_default()
            .into_iter()
            .map(|row| (row.id.0, row.attempts))
            .collect()
    }

    /// The open pending requests of `name` that are new since `before` or were asked for again
    /// since: the hosts the install waited on, by request id.
    fn waited_on(&self, name: &WorkspaceId, before: &BTreeMap<i64, u64>) -> BTreeMap<i64, String> {
        self.inner
            .store
            .open_pending_every_user(&self.inner.all_users, Some(name))
            .unwrap_or_default()
            .into_iter()
            .filter(|row| before.get(&row.id.0).is_none_or(|was| row.attempts > *was))
            .map(|row| (row.id.0, row.host.to_string()))
            .collect()
    }

    /// The problem for an install that left tools missing.
    fn install_problem(
        &self,
        name: &WorkspaceId,
        failed: &BTreeSet<String>,
        reason: &str,
        hosts: &[String],
    ) -> Problem {
        let what = if failed.is_empty() {
            "The tool install failed".to_owned()
        } else {
            format!(
                "mise did not install {}",
                failed.iter().cloned().collect::<Vec<_>>().join(", ")
            )
        };
        let mut detail = format!(
            "{what}: {reason}. The workspace works without them; run `mise install` in a terminal \
             in the workspace to see the whole output."
        );
        if !hosts.is_empty() {
            detail.push_str(" The install asked for hosts no rule allows yet: ");
            detail.push_str(&hosts.join(", "));
            detail.push_str(". Allow them on the Network tab and Puddle installs again.");
        }
        Problem::new(
            tools_key(name),
            format!("Puddle could not install the tools of {}", self.label(name)),
            detail,
        )
        .about(name.clone())
    }

    fn tools_event(&self, name: &WorkspaceId, state: ToolsState) {
        self.inner.events.emit(Event::WorkspaceTools {
            workspace: name.clone(),
            state,
        });
    }
}

#[cfg(test)]
mod tests;
