// SPDX-License-Identifier: AGPL-3.0-or-later
//! The filtered SSH agent on the wire: the hosts a workspace may use the user's SSH agent for.

use puddle_types::WorkspaceId;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// One host a workspace may use the user's SSH agent for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SshAgentHostView {
    /// The host, as `ssh` in the workspace names it: a lower-case name or an IP address.
    pub host: String,
    /// The keys it may use, as SHA-256 fingerprints (`SHA256:...`, as `ssh-keygen -l` prints
    /// them), sorted; `null` for every key the agent holds.
    #[schema(required = true)]
    pub keys: Option<Vec<String>>,
    /// Epoch ms the agent was turned on for the host, or its keys last changed.
    pub changed_at: u64,
}

impl From<puddle_store::SshAgentHost> for SshAgentHostView {
    fn from(row: puddle_store::SshAgentHost) -> Self {
        Self {
            host: row.host,
            keys: row.keys,
            changed_at: row.changed_at,
        }
    }
}

/// The hosts a workspace may use the user's SSH agent for. Empty unless the user turned it on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SshAgentView {
    /// The workspace.
    pub workspace: WorkspaceId,
    /// The hosts, sorted by name.
    pub hosts: Vec<SshAgentHostView>,
}

/// A question the filtered SSH agent has put to the user and that waits for an answer:
/// the workspace's `ssh` is logging in to `host` and the agent would sign with `key`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SshAgentAskView {
    /// Question id, never reused.
    pub id: i64,
    /// The workspace whose `ssh` is waiting.
    pub workspace: WorkspaceId,
    /// The host the login is for: a normalised name or an IP address. Untrusted (the guest
    /// names it): escape it when rendering.
    pub host: String,
    /// The key's type, such as `ssh-ed25519`.
    pub key_type: String,
    /// The key's SHA-256 fingerprint (`SHA256:...`, as `ssh-keygen -l` prints it).
    pub fingerprint: String,
    /// Epoch ms of the first request.
    pub first_seen: u64,
    /// Epoch ms of the latest request.
    pub last_seen: u64,
    /// How many requests the question stands for.
    pub attempts: u64,
}

impl From<puddle_store::SshAgentAsk> for SshAgentAskView {
    fn from(ask: puddle_store::SshAgentAsk) -> Self {
        Self {
            id: ask.id,
            workspace: ask.workspace,
            host: ask.host,
            key_type: ask.key_type,
            fingerprint: ask.fingerprint,
            first_seen: ask.first_seen,
            last_seen: ask.last_seen,
            attempts: ask.attempts,
        }
    }
}

/// The open questions of the filtered SSH agent, most recent first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SshAgentAskList {
    /// The questions that wait for an answer.
    pub asks: Vec<SshAgentAskView>,
}

/// How the user answers a first-use question.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SshAgentAnswerKind {
    /// Sign for the logins waiting now; ask again next time.
    Once,
    /// Sign for this key at this host until the workspace stops.
    Run,
    /// Sign for this key at this host until the agent is turned off for the host.
    Always,
    /// Do not sign.
    Deny,
}

impl From<SshAgentAnswerKind> for puddle_store::SshAgentAnswer {
    fn from(kind: SshAgentAnswerKind) -> Self {
        match kind {
            SshAgentAnswerKind::Once => Self::Once,
            SshAgentAnswerKind::Run => Self::Run,
            SshAgentAnswerKind::Always => Self::Always,
            SshAgentAnswerKind::Deny => Self::Deny,
        }
    }
}

/// The user's answer to a first-use question.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SshAgentAnswerRequest {
    /// What the user decided.
    pub answer: SshAgentAnswerKind,
}

/// Which keys a host may use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SshAgentHostRequest {
    /// SHA-256 fingerprints (`SHA256:` and 43 base64 characters, at most 64 of them); absent or
    /// `null` for every key the agent holds.
    #[serde(default)]
    pub keys: Option<Vec<String>>,
}
