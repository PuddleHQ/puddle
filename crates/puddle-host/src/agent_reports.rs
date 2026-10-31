// SPDX-License-Identifier: GPL-3.0-or-later
//! The guest agents' reports about their own workspaces (an entrypoint that exited, stub DNS
//! trouble, an editor server that could not be installed or updated), made background problems.
//!
//! A report is the guest's claim and the guest is untrusted, so a report can only raise one of
//! four problems of the workspace whose route carried it, with text written here. Reports count
//! only between a start and a stop, and each run of a workspace changes each problem a bounded
//! number of times, whatever the guest sends: the entrypoint problem once, the DNS one once per
//! kind, each editor one at most [`MAX_EDITOR_CHANGES`] times.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, PoisonError};

use puddle_agent_proto::host::{AgentReport, AgentReports};
use puddle_agent_proto::{EditorProblemKind, StubDnsTrouble};
use puddle_types::{Problem, Problems, WorkspaceName};

/// Longest entrypoint failure (`<status>: <output>`) put in a problem.
const MAX_FAILURE_CHARS: usize = 320;

/// The key of the problem about the image's entrypoint of `name`.
fn entrypoint_key(name: &WorkspaceName) -> String {
    format!("entrypoint:{name}")
}

/// How often one run of a workspace may change the text of each editor problem.
const MAX_EDITOR_CHANGES: u8 = 4;

/// The key of the problem about the browser editor's install in `name`.
pub(crate) fn editor_install_key(name: &WorkspaceName) -> String {
    format!("editor-install:{name}")
}

/// The key of the problem about the browser editor's updates in `name`.
fn editor_update_key(name: &WorkspaceName) -> String {
    format!("editor-update:{name}")
}

/// The key of the problem about the stub DNS of `name`.
fn stub_dns_key(name: &WorkspaceName) -> String {
    format!("stub-dns:{name}")
}

/// The problem for an entrypoint that exited with a failure. `failure` is `<status>: <end of its
/// output>`, from the boot hook's watch or the agent's report, which say it the same way, so both
/// raise the same problem.
pub(crate) fn entrypoint_problem(name: &WorkspaceName, failure: &str) -> Problem {
    let failure: String = failure
        .chars()
        .take(MAX_FAILURE_CHARS)
        .map(|c| if c.is_control() { '?' } else { c })
        .collect();
    Problem::new(
        entrypoint_key(name),
        format!("The image's entrypoint in {name} stopped"),
        format!(
            "It exited with status {failure}. The workspace keeps running without it, so whatever \
             it should have started (a service or a daemon) is not running. Its full output is in \
             /run/puddle/entrypoint.log in the workspace; starting the workspace again runs it \
             again."
        ),
    )
    .about(name.clone())
}

/// The problem for an editor server that could not be installed (a refused download, or a network
/// that kept failing); `reason` is the agent's cleaned text.
fn editor_install_problem(name: &WorkspaceName, reason: &str) -> Problem {
    Problem::new(
        editor_install_key(name),
        format!("The browser editor could not be installed in {name}: {reason}"),
        "Puddle could not get the editor server for this workspace, so the browser editor cannot \
         open in it until an install works. Puddle tries again (after a refused download it waits \
         longer each time, up to an hour); stopping and starting the workspace tries at once. \
         Terminals and desktop VS Code are not affected."
            .to_owned(),
    )
    .about(name.clone())
}

/// The problem for a newer editor server build that could not be taken (refused, rolled back after
/// a bad start, or looks that keep failing); `reason` is the agent's cleaned text.
fn editor_update_problem(name: &WorkspaceName, reason: &str) -> Problem {
    Problem::new(
        editor_update_key(name),
        format!("The browser editor in {name} could not update: {reason}"),
        "The editor keeps working on the build it has, so nothing you do in it is affected. \
         Puddle looks for a newer build again later (after a refused one it waits longer each \
         time, up to an hour); this ends when a look works, or when the workspace stops."
            .to_owned(),
    )
    .about(name.clone())
}

/// What a kind of stub DNS trouble means for the tools in the workspace, or `None` when the client
/// asks again and usually gets its answer (only the host log hears of it).
fn stub_dns_effect(kind: StubDnsTrouble) -> Option<&'static str> {
    match kind {
        StubDnsTrouble::LookupFailed => {
            Some("some lookups failed because asking Puddle for them failed")
        }
        StubDnsTrouble::LookupTimedOut => {
            Some("some lookups failed because Puddle did not answer them in time")
        }
        StubDnsTrouble::NoStandInAddress => {
            Some("the resolver ran out of addresses to hand out, so lookups of new names failed")
        }
        StubDnsTrouble::RecordsLeftOut => {
            Some("some answers left out records the resolver cannot carry")
        }
        _ => None,
    }
}

fn stub_dns_problem(name: &WorkspaceName, kinds: &BTreeSet<StubDnsTrouble>) -> Problem {
    let effects: Vec<_> = kinds.iter().filter_map(|k| stub_dns_effect(*k)).collect();
    Problem::new(
        stub_dns_key(name),
        format!("Name lookups in {name} are failing"),
        format!(
            "Since the workspace started, {}. Tools in it may say a name does not exist or fail to \
             connect. The workspace's /run/puddle/agent.log names the lookups. This goes away when \
             the workspace stops or starts again.",
            effects.join("; ")
        ),
    )
    .about(name.clone())
}

/// What one run of a workspace has reported so far.
#[derive(Debug)]
struct Run {
    /// Reports count: the workspace is starting or running, not stopping.
    taking: bool,
    /// The entrypoint problem was raised from a report in this run.
    entrypoint: bool,
    /// The DNS trouble kinds the problem names.
    dns: BTreeSet<StubDnsTrouble>,
    /// The editor's install problem.
    install: EditorSlot,
    /// The editor's update problem.
    update: EditorSlot,
}

/// What one run has reported of one editor problem.
#[derive(Debug, Default)]
struct EditorSlot {
    /// The reason the problem names now, `None` while there is none.
    reason: Option<String>,
    /// How often the problem's text changed in this run.
    changes: u8,
}

/// What a report did to an [`EditorSlot`].
enum Slot {
    /// Nothing the user would see.
    Same,
    /// The problem is over.
    Ended,
    /// The problem has this reason now.
    Raised(String),
    /// The run already changed it [`MAX_EDITOR_CHANGES`] times.
    Limited,
}

impl EditorSlot {
    fn report(&mut self, problem: Option<String>) -> Slot {
        match problem {
            None if self.reason.take().is_some() => Slot::Ended,
            None => Slot::Same,
            Some(reason) if self.reason.as_deref() == Some(reason.as_str()) => Slot::Same,
            Some(_) if self.changes >= MAX_EDITOR_CHANGES => Slot::Limited,
            Some(reason) => {
                self.changes += 1;
                self.reason = Some(reason.clone());
                Slot::Raised(reason)
            }
        }
    }
}

impl Run {
    fn new() -> Self {
        Self {
            taking: true,
            entrypoint: false,
            dns: BTreeSet::new(),
            install: EditorSlot::default(),
            update: EditorSlot::default(),
        }
    }
}

/// Takes the reports of every workspace and keeps the two problems they can raise. The workspaces
/// service tells it when a run begins and ends.
pub(crate) struct ReportDesk {
    problems: Arc<Problems>,
    runs: Mutex<BTreeMap<WorkspaceName, Run>>,
}

impl ReportDesk {
    pub(crate) fn new(problems: Arc<Problems>) -> Self {
        Self {
            problems,
            runs: Mutex::new(BTreeMap::new()),
        }
    }

    fn runs(&self) -> std::sync::MutexGuard<'_, BTreeMap<WorkspaceName, Run>> {
        self.runs.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A start or create of `name` begins: its reports count from now, and what the last run
    /// raised ends (the boot hook raises the entrypoint problem again when it fails at once).
    pub(crate) fn begin(&self, name: &WorkspaceName) {
        self.runs().insert(name.clone(), Run::new());
        self.problems.clear(&entrypoint_key(name));
        self.problems.clear(&stub_dns_key(name));
        self.problems.clear(&editor_install_key(name));
        self.problems.clear(&editor_update_key(name));
    }

    /// A stop of `name` was accepted: what its guest reports from now (its entrypoint ended by the
    /// shutdown) does not count.
    pub(crate) fn pause(&self, name: &WorkspaceName) {
        if let Some(run) = self.runs().get_mut(name) {
            run.taking = false;
        }
    }

    /// The stop of `name` failed and it runs on: its reports count again.
    pub(crate) fn resume(&self, name: &WorkspaceName) {
        if let Some(run) = self.runs().get_mut(name) {
            run.taking = true;
        }
    }

    /// `name` no longer runs: the DNS and editor problems are about a machine that is gone (the
    /// agent of the next start tells again). The entrypoint problem stays until the next start,
    /// because the image still has the fault.
    pub(crate) fn end(&self, name: &WorkspaceName) {
        self.runs().remove(name);
        self.problems.clear(&stub_dns_key(name));
        self.problems.clear(&editor_install_key(name));
        self.problems.clear(&editor_update_key(name));
    }

    /// `name` was deleted: both problems end.
    pub(crate) fn forget(&self, name: &WorkspaceName) {
        self.end(name);
        self.problems.clear(&entrypoint_key(name));
    }
}

impl AgentReports for ReportDesk {
    fn report(&self, workspace: &WorkspaceName, report: AgentReport) {
        let mut end_editor = None;
        let raise = {
            let mut runs = self.runs();
            let Some(run) = runs.get_mut(workspace).filter(|run| run.taking) else {
                tracing::debug!(%workspace, "agent report for a workspace that is not starting or running ignored");
                return;
            };
            match report {
                AgentReport::EntrypointExited { status: 0, .. } => None,
                AgentReport::EntrypointExited { .. } if run.entrypoint => {
                    tracing::info!(%workspace, "a second entrypoint report in one run ignored");
                    None
                }
                AgentReport::EntrypointExited { status, tail } => {
                    run.entrypoint = true;
                    let tail = if tail.is_empty() {
                        "no output".to_owned()
                    } else {
                        tail
                    };
                    Some(entrypoint_problem(workspace, &format!("{status}: {tail}")))
                }
                AgentReport::StubDns { kind, .. } => (stub_dns_effect(kind).is_some()
                    && run.dns.insert(kind))
                .then(|| stub_dns_problem(workspace, &run.dns)),
                AgentReport::EditorStatus { kind, problem } => {
                    let slot = match kind {
                        EditorProblemKind::Install => &mut run.install,
                        EditorProblemKind::Update => &mut run.update,
                        _ => return,
                    };
                    match slot.report(problem) {
                        Slot::Same => None,
                        Slot::Ended => {
                            end_editor = Some(kind);
                            None
                        }
                        Slot::Limited => {
                            tracing::info!(%workspace, ?kind, "an editor report past the limit for one run ignored");
                            None
                        }
                        Slot::Raised(reason) if kind == EditorProblemKind::Install => {
                            Some(editor_install_problem(workspace, &reason))
                        }
                        Slot::Raised(reason) => Some(editor_update_problem(workspace, &reason)),
                    }
                }
                _ => None,
            }
        };
        match end_editor {
            Some(EditorProblemKind::Install) => {
                self.problems.clear(&editor_install_key(workspace));
            }
            Some(_) => self.problems.clear(&editor_update_key(workspace)),
            None => {}
        }
        if let Some(problem) = raise {
            self.problems.raise(problem);
        }
    }
}

#[cfg(test)]
mod tests;
