// SPDX-License-Identifier: AGPL-3.0-or-later
//! The host's side of the filtered SSH agent: which agent on this machine it asks, and which hosts
//! each workspace may use it for.
//!
//! The agent is looked up again for every request, from the settings (`ui.ssh_agent_pipe`) or, with
//! none named, the platform default (the OpenSSH pipe on Windows, this process's `SSH_AUTH_SOCK`
//! elsewhere), so a changed setting applies to the next login with no restart. The hosts come from
//! the store, where only the user's own change puts one.

use std::collections::HashMap;
use std::io;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use puddle_api::SettingsRepo;
use puddle_proxy::ssh_agent::{AgentAt, AgentGate, HostAgent};
use puddle_ssh_agent::filter::{
    AllowedKeys, Disapproval, Outcome, PinFailure, Refusal, fingerprint, key_type,
};
use puddle_ssh_agent::{AgentEndpoint, AgentError, Identity};
use puddle_store::{
    Actor, HostKeyPin, NewWorkspaceOp, SshAgentAskStart, SshAgentHost, Store, WorkspaceOpKind,
    WorkspaceOpOutcome,
};
use puddle_types::{Host, WorkspaceId};

/// The user's SSH agent, as the settings name it at the moment of each request.
pub(crate) struct SettingsAgent {
    settings: Option<Arc<dyn SettingsRepo>>,
    /// The `SSH_AUTH_SOCK` used when the settings name no agent (outside Windows).
    auth_sock: Option<std::ffi::OsString>,
}

impl SettingsAgent {
    /// The agent `settings` name, else the platform's default.
    pub(crate) fn new(settings: Option<Arc<dyn SettingsRepo>>) -> Self {
        Self {
            settings,
            auth_sock: std::env::var_os("SSH_AUTH_SOCK"),
        }
    }

    fn agent(&self) -> Result<AgentAt, AgentError> {
        let configured = self
            .settings
            .as_deref()
            .and_then(crate::doctor::ssh_agent_pipe);
        AgentEndpoint::resolve(
            cfg!(windows),
            configured.as_deref(),
            self.auth_sock.as_deref(),
        )
        .map(AgentAt::new)
        .ok_or_else(|| {
            AgentError::Unreachable(io::Error::new(
                io::ErrorKind::NotFound,
                "no SSH agent: SSH_AUTH_SOCK is not set and the settings name none",
            ))
        })
    }
}

impl HostAgent for SettingsAgent {
    fn list(&self) -> Result<Vec<Identity>, AgentError> {
        self.agent()?.list()
    }

    fn sign(&self, key: &[u8], data: &[u8], flags: u32) -> Result<Vec<u8>, AgentError> {
        self.agent()?.sign(key, data, flags)
    }
}

/// How long a login waits for the user's first-use answer before it goes on without the agent.
/// The question stays open: an "always" given later serves the next try.
pub(crate) const ANSWER_WAIT: Duration = Duration::from_secs(120);

/// How often a waiting login looks for the answer.
const ANSWER_POLL: Duration = Duration::from_millis(250);

/// The same refusal of one workspace is written to the audit log once a minute: a guest that
/// retries a refused request cannot fill it.
const REFUSAL_RECORD_EVERY: Duration = Duration::from_secs(60);

/// Most distinct refusals remembered for that rule.
const REFUSALS_REMEMBERED: usize = 256;

/// The first-use question and the audit record of the filtered agent, both kept in the
/// store: the question is a row the inbox shows, and the user's answer is a row change the waiting
/// login polls for, so an answer from the UI, the API or the command line all reach it alike.
pub(crate) struct StoreGate {
    store: Arc<Store>,
    wait: Duration,
    poll: Duration,
    refusal_every: Duration,
    refused: Mutex<HashMap<(WorkspaceId, String), Instant>>,
}

impl StoreGate {
    /// A gate over `store`. Run grants and questions left by a Puddle that exited are dropped:
    /// no workspace runs at this point.
    pub(crate) fn new(store: Arc<Store>) -> Self {
        Self::with_timing(store, ANSWER_WAIT, ANSWER_POLL, REFUSAL_RECORD_EVERY)
    }

    fn with_timing(
        store: Arc<Store>,
        wait: Duration,
        poll: Duration,
        refusal_every: Duration,
    ) -> Self {
        if let Err(error) = store.end_ssh_agent_run(None) {
            tracing::warn!(%error, "SSH agent run grants of an earlier run were not cleared");
        }
        Self {
            store,
            wait,
            poll,
            refusal_every,
            refused: Mutex::new(HashMap::new()),
        }
    }

    fn note(&self, workspace: &WorkspaceId, op: WorkspaceOpKind, ok: bool, detail: String) {
        self.store.note_workspace_op(&NewWorkspaceOp {
            op,
            workspace: Some(workspace.clone()),
            user: None,
            outcome: if ok {
                WorkspaceOpOutcome::Succeeded
            } else {
                WorkspaceOpOutcome::Failed
            },
            actor: Actor::System,
            detail: Some(detail),
        });
    }

    /// Whether this refusal is new enough to write down.
    fn worth_recording(&self, workspace: &WorkspaceId, cause: String) -> bool {
        let now = Instant::now();
        let mut seen = self.refused.lock().unwrap_or_else(PoisonError::into_inner);
        if seen.len() >= REFUSALS_REMEMBERED {
            seen.retain(|_, at| now.duration_since(*at) < self.refusal_every);
        }
        let key = (workspace.clone(), cause);
        if seen
            .get(&key)
            .is_some_and(|at| now.duration_since(*at) < self.refusal_every)
        {
            return false;
        }
        seen.insert(key, now);
        true
    }
}

impl AgentGate for StoreGate {
    fn pin_host_key(
        &self,
        workspace: &WorkspaceId,
        host: &str,
        host_key: &str,
    ) -> Result<(), PinFailure> {
        match self.store.pin_ssh_agent_host_key(workspace, host, host_key) {
            Ok(HostKeyPin::Pinned | HostKeyPin::Same) => Ok(()),
            Ok(HostKeyPin::Other(kept)) => Err(PinFailure::Changed(kept)),
            // Turned off between the switch read and here: nothing to bind to.
            Ok(HostKeyPin::Off) => Err(PinFailure::Unavailable),
            Err(error) => {
                tracing::warn!(workspace = %workspace, host, %error, "the SSH agent host key could not be checked");
                Err(PinFailure::Unavailable)
            }
        }
    }

    fn approve(&self, workspace: &WorkspaceId, host: &str, key: &[u8]) -> Result<(), Disapproval> {
        let (Some(fingerprint), Some(kind)) = (fingerprint(key), key_type(key)) else {
            return Err(Disapproval::Unavailable);
        };
        let id = match self
            .store
            .start_ssh_agent_ask(workspace, host, &fingerprint, &kind)
        {
            Ok(SshAgentAskStart::Granted) => return Ok(()),
            Ok(SshAgentAskStart::Open(id)) => id,
            Err(error) => {
                tracing::warn!(workspace = %workspace, host, %error, "the SSH agent question could not be put");
                return Err(Disapproval::Unavailable);
            }
        };
        let deadline = Instant::now() + self.wait;
        loop {
            match self.store.ssh_agent_ask(id) {
                Ok(Some(ask)) if ask.state.allows() => return Ok(()),
                Ok(Some(ask)) if ask.state != puddle_store::SshAgentAskState::Requested => {
                    return Err(Disapproval::Denied);
                }
                // Gone: the workspace stopped under the question.
                Ok(None) => return Err(Disapproval::Unanswered),
                Ok(Some(_)) => {}
                Err(error) => {
                    tracing::warn!(workspace = %workspace, host, %error, "the SSH agent answer could not be read");
                    return Err(Disapproval::Unavailable);
                }
            }
            if Instant::now() >= deadline {
                return Err(Disapproval::Unanswered);
            }
            std::thread::sleep(self.poll);
        }
    }

    fn record(&self, workspace: &WorkspaceId, host: Option<&str>, outcome: &Outcome) {
        let host = host.unwrap_or("-");
        match outcome {
            Outcome::Signed {
                family,
                fingerprint,
            } => self.note(
                workspace,
                WorkspaceOpKind::SshAgentSigned,
                true,
                format!("signed a login to {host} with {family} key {fingerprint}"),
            ),
            // Every ssh without a session bind lists keys first; that is no event.
            Outcome::Refused(Refusal::NotBound) | Outcome::Bound(_) | Outcome::Listed(_) => {}
            Outcome::Refused(why) => {
                let detail = format!("refused for {host}: {why}");
                if self.worth_recording(workspace, detail.clone()) {
                    self.note(workspace, WorkspaceOpKind::SshAgentRefused, false, detail);
                }
            }
        }
    }
}

/// The hosts of `rows` as the proxy takes them. A stored host that no longer parses is left out
/// and logged: the API stores only valid ones, so this only meets a row from a newer Puddle.
pub(crate) fn allowed_hosts(rows: Vec<SshAgentHost>) -> HashMap<Host, AllowedKeys> {
    rows.into_iter()
        .filter_map(|row| match Host::parse_normalised(&row.host) {
            Ok(host) => {
                let keys = row.keys.map_or(AllowedKeys::All, |k| {
                    AllowedKeys::Only(k.into_iter().collect())
                });
                Some((host, keys))
            }
            Err(e) => {
                tracing::warn!(host = %row.host, error = %e, "an SSH agent host is not valid");
                None
            }
        })
        .collect()
}

#[cfg(test)]
mod tests;
