// SPDX-License-Identifier: GPL-3.0-or-later
//! The guest agents' reports about their own workspaces (an entrypoint that exited, stub DNS
//! trouble, an editor server that could not be installed or updated, capture rules the guest tore
//! down or that cannot be installed, an agent that speaks another control protocol), made
//! background problems.
//!
//! A report is the guest's claim and the guest is untrusted, so a report can only raise one of
//! four problems of the workspace whose route carried it, with text written here. Reports count
//! only between a start and a stop, and each run of a workspace changes each problem a bounded
//! number of times, whatever the guest sends: the entrypoint problem once, the DNS one once per
//! kind, each editor one at most [`MAX_REASON_CHANGES`] times.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, PoisonError};

use puddle_agent_proto::control::PROTOCOL_VERSION;
use puddle_agent_proto::editor::NEEDS_GLIBC;
use puddle_agent_proto::host::{AgentReport, AgentReports};
use puddle_agent_proto::{EditorProblemKind, StubDnsTrouble};
use puddle_api::DisplayName;
use puddle_types::{Problem, Problems, WorkspaceId};

/// Longest entrypoint failure (`<status>: <output>`) put in a problem.
const MAX_FAILURE_CHARS: usize = 320;

/// The key of the problem about the image's entrypoint of `name`.
fn entrypoint_key(id: &WorkspaceId) -> String {
    format!("entrypoint:{id}")
}

/// How often one run of a workspace may change the text of each problem that names a reason.
const MAX_REASON_CHANGES: u8 = 4;

/// The key of the problem about the browser editor's install in `name`.
pub(crate) fn editor_install_key(id: &WorkspaceId) -> String {
    format!("editor-install:{id}")
}

/// The key of the problem about the browser editor's updates in `name`.
fn editor_update_key(id: &WorkspaceId) -> String {
    format!("editor-update:{id}")
}

/// The key of the problem about the browser editor's session user in `name`.
fn editor_session_user_key(id: &WorkspaceId) -> String {
    format!("editor-session-user:{id}")
}

/// The key of the problem about capture rules the guest tore down in `name`.
fn capture_tamper_key(id: &WorkspaceId) -> String {
    format!("capture-tamper:{id}")
}

/// The key of the problem about capture rules that cannot be installed in `name`.
fn capture_unavailable_key(id: &WorkspaceId) -> String {
    format!("capture-unavailable:{id}")
}

/// The key of the problem about an agent in `name` that speaks another control protocol.
fn agent_protocol_key(id: &WorkspaceId) -> String {
    format!("agent-protocol:{id}")
}

/// The key of the problem about the stub DNS of `name`.
fn stub_dns_key(id: &WorkspaceId) -> String {
    format!("stub-dns:{id}")
}

/// The key of the problem about an image with no `sudo` for `name`'s session user.
fn no_sudo_key(id: &WorkspaceId) -> String {
    format!("no-sudo:{id}")
}

/// The problem for a workspace whose session user (not root) cannot become root because the image
/// has no `sudo`. The boot hook sees it on every boot, so the next boot that finds `sudo` (or a
/// root session user) clears it ([`clear_no_sudo`]).
pub(crate) fn no_sudo_problem(id: &WorkspaceId, name: &str) -> Problem {
    Problem::new(
        no_sudo_key(id),
        format!("{name} has no sudo, so its user cannot become root"),
        "Puddle runs the workspace as the session user `agent`, and `agent` becomes root with \
         `sudo`. This image has none, so `sudo` and `sudo -i` fail in a terminal, and so does \
         anything that installs system packages. To fix it, use an image that has `sudo`, or \
         switch the session user to root on the workspace's Settings tab and start the workspace \
         again. This ends at the next start that finds `sudo`."
            .to_owned(),
    )
    .about(id.clone())
}

/// A boot found `sudo`, or no session user that needs it: the problem of an earlier boot ends.
pub(crate) fn clear_no_sudo(problems: &Problems, id: &WorkspaceId) {
    problems.clear(&no_sudo_key(id));
}

/// The problem for an entrypoint that exited with a failure. `failure` is `<status>: <end of its
/// output>`, from the boot hook's watch or the agent's report, which say it the same way, so both
/// raise the same problem.
pub(crate) fn entrypoint_problem(id: &WorkspaceId, name: &str, failure: &str) -> Problem {
    let failure: String = failure
        .chars()
        .take(MAX_FAILURE_CHARS)
        .map(|c| if c.is_control() { '?' } else { c })
        .collect();
    Problem::new(
        entrypoint_key(id),
        format!("The image's entrypoint in {name} stopped"),
        format!(
            "It exited with status {failure}. The workspace keeps running without it, so whatever \
             it should have started (a service or a daemon) is not running. Its full output is in \
             /run/puddle/entrypoint.log in the workspace; starting the workspace again runs it \
             again."
        ),
    )
    .about(id.clone())
}

/// The problem for an editor server that could not be installed (a refused download, or a network
/// that kept failing); `reason` is the agent's cleaned text.
fn editor_install_problem(id: &WorkspaceId, name: &str, reason: &str) -> Problem {
    if reason == NEEDS_GLIBC {
        return Problem::new(
            editor_install_key(id),
            format!("The browser editor is not available in {name}: its image uses musl"),
            "The browser editor's server needs glibc, and this image uses musl (for example \
             Alpine), so the editor cannot open in this workspace. Use desktop VS Code over \
             direct SSH, or a workspace image with glibc. Terminals and desktop VS Code are not \
             affected."
                .to_owned(),
        )
        .about(id.clone());
    }
    Problem::new(
        editor_install_key(id),
        format!("The browser editor could not be installed in {name}: {reason}"),
        "Puddle could not get the editor server for this workspace, so the browser editor cannot \
         open in it until an install works. Puddle tries again (after a refused download it waits \
         longer each time, up to an hour); stopping and starting the workspace tries at once. \
         Terminals and desktop VS Code are not affected."
            .to_owned(),
    )
    .about(id.clone())
}

/// The problem for a newer editor server build that could not be taken (refused, rolled back after
/// a bad start, or looks that keep failing); `reason` is the agent's cleaned text.
fn editor_update_problem(id: &WorkspaceId, name: &str, reason: &str) -> Problem {
    Problem::new(
        editor_update_key(id),
        format!("The browser editor in {name} could not update: {reason}"),
        "The editor keeps working on the build it has, so nothing you do in it is affected. \
         Puddle looks for a newer build again later (after a refused one it waits longer each \
         time, up to an hour); this ends when a look works, or when the workspace stops."
            .to_owned(),
    )
    .about(id.clone())
}

/// The problem for an editor server that runs as root because the session user is missing in the
/// guest; `reason` is the agent's cleaned text.
fn editor_session_user_problem(id: &WorkspaceId, name: &str, reason: &str) -> Problem {
    Problem::new(
        editor_session_user_key(id),
        format!("The browser editor in {name} runs as root: {reason}"),
        "The editor opens and works, but its terminal and the files it writes belong to root \
         instead of the workspace's session user, because the guest has no such user (the boot \
         hook adds it and stops the start when it cannot, so it was removed or cannot be read \
         afterwards). Only the browser editor is affected: terminals opened from Puddle and \
         desktop VS Code are not. To get the user back, use an image that keeps the user (or switch the workspace's \
         session user to root in its settings) and start the workspace again; this ends when a start \
         finds the user, or when the workspace stops."
            .to_owned(),
    )
    .about(id.clone())
}

/// The problem for capture rules the guest deleted or edited; the agent put them back each time.
/// No count in the text: a moving count would bring the notice back after the user dismissed it
/// (the host log has every event).
fn capture_tamper_problem(id: &WorkspaceId, name: &str) -> Problem {
    Problem::new(
        capture_tamper_key(id),
        format!("Something in {name} deleted Puddle's capture rules; Puddle put them back"),
        "Since the workspace started, a process in it removed or changed the firewall rules that \
         send its network traffic through Puddle. Puddle restored them each time, so tools that \
         ignore proxy settings kept working through the proxy. A tool that flushes the firewall \
         (an `nft flush ruleset` in a setup script, a firewall manager) does this. This goes away \
         when the workspace stops or starts again."
            .to_owned(),
    )
    .about(id.clone())
}

/// The problem for capture rules that cannot be installed; `reason` is the agent's cleaned text.
fn capture_unavailable_problem(id: &WorkspaceId, name: &str, reason: &str) -> Problem {
    Problem::new(
        capture_unavailable_key(id),
        format!("Tools in {name} that ignore proxy settings are offline: {reason}"),
        "Puddle could not install the rules that send the workspace's network traffic through it, \
         so tools that ignore the HTTP proxy settings cannot reach the network. Tools that use the \
         settings are not affected. Puddle keeps trying; this goes away when the rules install, or \
         when the workspace stops or starts again."
            .to_owned(),
    )
    .about(id.clone())
}

/// The problem for a guest agent whose hello named another control protocol: the session ignores
/// it from then on. `agent_version` is the agent's cleaned text.
fn agent_protocol_problem(
    id: &WorkspaceId,
    name: &str,
    agent_version: &str,
    protocol: u32,
) -> Problem {
    let version = if agent_version.is_empty() {
        "an unnamed version".to_owned()
    } else {
        format!("version {agent_version}")
    };
    Problem::new(
        agent_protocol_key(id),
        format!("{name} runs a guest agent this Puddle cannot talk to"),
        format!(
            "The agent in the workspace ({version}) speaks control protocol {protocol}, and this \
             Puddle speaks {PROTOCOL_VERSION}. Puddle ignores what that agent reports and sends it \
             nothing, so open ports, out-of-memory kills and editor and capture problems in this \
             workspace go unreported. Network traffic still goes through Puddle and its rules. Stop \
             and start the workspace: a start gives it the agent that came with this Puddle. This \
             goes away when the workspace stops."
        ),
    )
    .about(id.clone())
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

fn stub_dns_problem(id: &WorkspaceId, name: &str, kinds: &BTreeSet<StubDnsTrouble>) -> Problem {
    let effects: Vec<_> = kinds.iter().filter_map(|k| stub_dns_effect(*k)).collect();
    Problem::new(
        stub_dns_key(id),
        format!("Name lookups in {name} are failing"),
        format!(
            "Since the workspace started, {}. Tools in it may say a name does not exist or fail to \
             connect. The workspace's /run/puddle/agent.log names the lookups. This goes away when \
             the workspace stops or starts again.",
            effects.join("; ")
        ),
    )
    .about(id.clone())
}

/// What one run of a workspace has reported so far.
#[derive(Debug)]
struct Run {
    /// What the user calls the workspace, for the text of its problems.
    name: DisplayName,
    /// Reports count: the workspace is starting or running, not stopping.
    taking: bool,
    /// The problems raised at most once per run that this run raised.
    raised: BTreeSet<Once>,
    /// The DNS trouble kinds the problem names.
    dns: BTreeSet<StubDnsTrouble>,
    /// The editor's install problem.
    install: ReasonSlot,
    /// The editor's update problem.
    update: ReasonSlot,
    /// The editor's session-user problem.
    session_user: ReasonSlot,
    /// The capture-unavailable problem.
    unavailable: ReasonSlot,
}

/// A problem a run raises at most once.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Once {
    /// The entrypoint exited with a failure.
    Entrypoint,
    /// The guest tore down the capture rules.
    Tampered,
    /// The agent speaks another control protocol.
    Mismatch,
}

/// What one run has reported of one problem that names a reason.
#[derive(Debug, Default)]
struct ReasonSlot {
    /// The reason the problem names now, `None` while there is none.
    reason: Option<String>,
    /// How often the problem's text changed in this run.
    changes: u8,
}

/// What a report did to a [`ReasonSlot`].
enum Slot {
    /// Nothing the user would see.
    Same,
    /// The problem is over.
    Ended,
    /// The problem has this reason now.
    Raised(String),
    /// The run already changed it [`MAX_REASON_CHANGES`] times.
    Limited,
}

impl ReasonSlot {
    fn report(&mut self, problem: Option<String>) -> Slot {
        match problem {
            None if self.reason.take().is_some() => Slot::Ended,
            None => Slot::Same,
            Some(reason) if self.reason.as_deref() == Some(reason.as_str()) => Slot::Same,
            Some(_) if self.changes >= MAX_REASON_CHANGES => Slot::Limited,
            Some(reason) => {
                self.changes += 1;
                self.reason = Some(reason.clone());
                Slot::Raised(reason)
            }
        }
    }
}

impl Run {
    fn new(name: DisplayName) -> Self {
        Self {
            name,
            taking: true,
            raised: BTreeSet::new(),
            dns: BTreeSet::new(),
            install: ReasonSlot::default(),
            update: ReasonSlot::default(),
            session_user: ReasonSlot::default(),
            unavailable: ReasonSlot::default(),
        }
    }

    /// `true` the first time this run raises `problem`.
    fn first(&mut self, problem: Once) -> bool {
        self.raised.insert(problem)
    }

    /// The problem an editor report raises, if any; sets `ended` when it ends one.
    fn editor(
        &mut self,
        workspace: &WorkspaceId,
        kind: EditorProblemKind,
        problem: Option<String>,
        ended: &mut Option<EditorProblemKind>,
    ) -> Option<Problem> {
        let shown = self.name.as_str().to_owned();
        let slot = match kind {
            EditorProblemKind::Install => &mut self.install,
            EditorProblemKind::Update => &mut self.update,
            EditorProblemKind::SessionUser => &mut self.session_user,
            _ => return None,
        };
        match slot.report(problem) {
            Slot::Same => None,
            Slot::Ended => {
                *ended = Some(kind);
                None
            }
            Slot::Limited => {
                tracing::info!(%workspace, ?kind, "an editor report past the limit for one run ignored");
                None
            }
            Slot::Raised(reason) if kind == EditorProblemKind::Install => {
                Some(editor_install_problem(workspace, &shown, &reason))
            }
            Slot::Raised(reason) if kind == EditorProblemKind::Update => {
                Some(editor_update_problem(workspace, &shown, &reason))
            }
            Slot::Raised(reason) => Some(editor_session_user_problem(workspace, &shown, &reason)),
        }
    }
}

/// Takes the reports of every workspace and keeps the two problems they can raise. The workspaces
/// service tells it when a run begins and ends.
pub(crate) struct ReportDesk {
    problems: Arc<Problems>,
    runs: Mutex<BTreeMap<WorkspaceId, Run>>,
}

impl ReportDesk {
    pub(crate) fn new(problems: Arc<Problems>) -> Self {
        Self {
            problems,
            runs: Mutex::new(BTreeMap::new()),
        }
    }

    fn runs(&self) -> std::sync::MutexGuard<'_, BTreeMap<WorkspaceId, Run>> {
        self.runs.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A start or create of `id` begins: its reports count from now, and what the last run
    /// raised ends (the boot hook raises the entrypoint problem again when it fails at once).
    pub(crate) fn begin(&self, id: &WorkspaceId, name: &DisplayName) {
        self.runs().insert(id.clone(), Run::new(name.clone()));
        self.problems.clear(&entrypoint_key(id));
        self.problems.clear(&stub_dns_key(id));
        self.problems.clear(&editor_install_key(id));
        self.problems.clear(&editor_update_key(id));
        self.problems.clear(&editor_session_user_key(id));
        self.problems.clear(&capture_tamper_key(id));
        self.problems.clear(&capture_unavailable_key(id));
        self.problems.clear(&agent_protocol_key(id));
    }

    /// A stop of `id` was accepted: what its guest reports from now (its entrypoint ended by the
    /// shutdown) does not count.
    pub(crate) fn pause(&self, id: &WorkspaceId) {
        if let Some(run) = self.runs().get_mut(id) {
            run.taking = false;
        }
    }

    /// The stop of `id` failed and it runs on: its reports count again.
    pub(crate) fn resume(&self, id: &WorkspaceId) {
        if let Some(run) = self.runs().get_mut(id) {
            run.taking = true;
        }
    }

    /// `id` no longer runs: the DNS, editor, capture and agent-protocol problems are about a machine that is gone (the
    /// agent of the next start tells again). The entrypoint problem stays until the next start,
    /// because the image still has the fault.
    pub(crate) fn end(&self, id: &WorkspaceId) {
        self.runs().remove(id);
        self.problems.clear(&stub_dns_key(id));
        self.problems.clear(&editor_install_key(id));
        self.problems.clear(&editor_update_key(id));
        self.problems.clear(&editor_session_user_key(id));
        self.problems.clear(&capture_tamper_key(id));
        self.problems.clear(&capture_unavailable_key(id));
        self.problems.clear(&agent_protocol_key(id));
    }

    /// `id` was deleted: both problems end.
    pub(crate) fn forget(&self, id: &WorkspaceId) {
        self.end(id);
        self.problems.clear(&entrypoint_key(id));
        clear_no_sudo(&self.problems, id);
    }
}

impl AgentReports for ReportDesk {
    fn report(&self, workspace: &WorkspaceId, report: AgentReport) {
        let mut end_editor = None;
        let mut end_unavailable = false;
        let raise = {
            let mut runs = self.runs();
            let Some(run) = runs.get_mut(workspace).filter(|run| run.taking) else {
                tracing::debug!(%workspace, "agent report for a workspace that is not starting or running ignored");
                return;
            };
            let shown = run.name.as_str().to_owned();
            match report {
                AgentReport::EntrypointExited { status: 0, .. } => None,
                AgentReport::EntrypointExited { .. } if run.raised.contains(&Once::Entrypoint) => {
                    tracing::info!(%workspace, "a second entrypoint report in one run ignored");
                    None
                }
                AgentReport::EntrypointExited { status, tail } => {
                    run.first(Once::Entrypoint);
                    let tail = if tail.is_empty() {
                        "no output".to_owned()
                    } else {
                        tail
                    };
                    Some(entrypoint_problem(
                        workspace,
                        &shown,
                        &format!("{status}: {tail}"),
                    ))
                }
                AgentReport::StubDns { kind, .. } => (stub_dns_effect(kind).is_some()
                    && run.dns.insert(kind))
                .then(|| stub_dns_problem(workspace, &shown, &run.dns)),
                AgentReport::EditorStatus { kind, problem } => {
                    run.editor(workspace, kind, problem, &mut end_editor)
                }
                AgentReport::CaptureTampered { .. } => run
                    .first(Once::Tampered)
                    .then(|| capture_tamper_problem(workspace, &shown)),
                AgentReport::CaptureUnavailable { reason } => {
                    match run.unavailable.report(reason) {
                        Slot::Same => None,
                        Slot::Ended => {
                            end_unavailable = true;
                            None
                        }
                        Slot::Limited => {
                            tracing::info!(%workspace, "a capture-unavailable report past the limit for one run ignored");
                            None
                        }
                        Slot::Raised(reason) => {
                            Some(capture_unavailable_problem(workspace, &shown, &reason))
                        }
                    }
                }
                AgentReport::ProtocolMismatch {
                    agent_version,
                    protocol,
                } => run
                    .first(Once::Mismatch)
                    .then(|| agent_protocol_problem(workspace, &shown, &agent_version, protocol)),
                _ => None,
            }
        };
        if end_unavailable {
            self.problems.clear(&capture_unavailable_key(workspace));
        }
        match end_editor {
            Some(EditorProblemKind::Install) => {
                self.problems.clear(&editor_install_key(workspace));
            }
            Some(EditorProblemKind::Update) => {
                self.problems.clear(&editor_update_key(workspace));
            }
            Some(_) => self.problems.clear(&editor_session_user_key(workspace)),
            None => {}
        }
        if let Some(problem) = raise {
            self.problems.raise(problem);
        }
    }
}

#[cfg(test)]
mod tests;
