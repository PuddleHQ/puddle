// SPDX-License-Identifier: AGPL-3.0-or-later
//! The filtered SSH agent's host side: what it learns from spliced SSH connections and how it
//! answers the guest's agent streams.
//!
//! - **Seen sessions.** On a `CONNECT` that announces SSH to a host the workspace has the agent on
//!   for, the tunnel feeds the server's bytes to a [`KexObserver`]; the host key and
//!   signature of the first key exchange are kept, with the host's name, for as long as the tunnel
//!   lasts. A session bind from the guest holds only for one of these.
//! - **Agent streams.** Each client of the guest's agent socket is one `ssh-agent` stream; every
//!   message on it goes through the workspace's [`Filter`], which asks the host's agent
//!   ([`HostAgent`]) only for what it lets through. The workspace's hosts and their allowed keys
//!   come from its termination ([`crate::Termination::with_ssh_agent_hosts`]), read on each request,
//!   so a switch turned off takes effect at once.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use puddle_ssh_agent::filter::{AllowedKeys, Context, Filter, Outcome};
use puddle_ssh_agent::kex::{KexObserver, SeenExchange};
use puddle_ssh_agent::wire::MAX_MESSAGE;
use puddle_ssh_agent::{AgentEndpoint, AgentError, Identity};
use puddle_types::{Host, WorkspaceId};
use ssh_key::public::KeyData;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::terminate::TerminationSource;

/// How long an agent stream may sit between two requests before it is closed. `ssh` keeps its
/// agent connection open while it logs in, which includes waiting for a password or a prompt.
pub const IDLE_LIMIT: Duration = Duration::from_secs(600);

/// The user's own SSH agent on this machine, as the filter uses it.
pub trait HostAgent: Send + Sync {
    /// The keys it holds.
    ///
    /// # Errors
    ///
    /// Whatever the agent fails with.
    fn list(&self) -> Result<Vec<Identity>, AgentError>;
    /// A signature over `data` with `key`.
    ///
    /// # Errors
    ///
    /// Whatever the agent fails with.
    fn sign(&self, key: &[u8], data: &[u8], flags: u32) -> Result<Vec<u8>, AgentError>;
}

/// The agent at an endpoint, asked with a time limit per request.
#[derive(Debug, Clone)]
pub struct AgentAt {
    endpoint: AgentEndpoint,
    list_limit: Duration,
    sign_limit: Duration,
}

impl AgentAt {
    /// The agent at `endpoint`: a key list waits at most 2 seconds, a signature 60 (an agent may
    /// ask the user first, as 1Password and Bitwarden do).
    #[must_use]
    pub fn new(endpoint: AgentEndpoint) -> Self {
        Self {
            endpoint,
            list_limit: Duration::from_secs(2),
            sign_limit: Duration::from_secs(60),
        }
    }
}

impl HostAgent for AgentAt {
    fn list(&self) -> Result<Vec<Identity>, AgentError> {
        puddle_ssh_agent::list_identities(&self.endpoint, self.list_limit)
    }

    fn sign(&self, key: &[u8], data: &[u8], flags: u32) -> Result<Vec<u8>, AgentError> {
        puddle_ssh_agent::sign(&self.endpoint, key, data, flags, self.sign_limit)
    }
}

/// One key exchange seen on a live tunnel.
#[derive(Debug)]
struct Seen {
    id: u64,
    workspace: WorkspaceId,
    host: Host,
    exchange: SeenExchange,
}

/// The filtered agent of every workspace: the host's agent and the sessions seen so far.
pub struct SshAgents {
    agent: Arc<dyn HostAgent>,
    seen: Mutex<Vec<Seen>>,
    next_id: AtomicU64,
}

impl std::fmt::Debug for SshAgents {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SshAgents").finish_non_exhaustive()
    }
}

impl SshAgents {
    /// The filtered agent in front of `agent`.
    #[must_use]
    pub fn new(agent: Arc<dyn HostAgent>) -> Self {
        Self {
            agent,
            seen: Mutex::new(Vec::new()),
            next_id: AtomicU64::new(0),
        }
    }

    fn sessions(&self) -> std::sync::MutexGuard<'_, Vec<Seen>> {
        // A panic while holding the lock leaves only whole entries behind.
        self.seen.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn add(&self, workspace: &WorkspaceId, host: &Host, exchange: SeenExchange) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.sessions().push(Seen {
            id,
            workspace: workspace.clone(),
            host: host.clone(),
            exchange,
        });
        id
    }

    fn remove(&self, id: u64) {
        self.sessions().retain(|s| s.id != id);
    }

    fn find(&self, workspace: &WorkspaceId, host_key: &KeyData, signature: &[u8]) -> Option<Host> {
        self.sessions()
            .iter()
            .find(|s| {
                s.workspace == *workspace
                    && s.exchange.host_key == *host_key
                    && s.exchange.signature == signature
            })
            .map(|s| s.host.clone())
    }

    /// How many sessions are kept now (for tests).
    #[cfg(feature = "testing")]
    #[must_use]
    pub fn seen_sessions(&self) -> usize {
        self.sessions().len()
    }
}

/// Watches the server's side of one spliced SSH tunnel for its key exchange, and keeps what it
/// saw for as long as the watch lives.
pub(crate) struct KexWatch {
    agents: Arc<SshAgents>,
    workspace: WorkspaceId,
    host: Host,
    observer: KexObserver,
    id: Option<u64>,
}

impl KexWatch {
    pub(crate) fn new(agents: Arc<SshAgents>, workspace: WorkspaceId, host: Host) -> Self {
        Self {
            agents,
            workspace,
            host,
            observer: KexObserver::new(),
            id: None,
        }
    }

    /// Takes the next bytes the server sent toward the guest.
    pub(crate) fn feed(&mut self, bytes: &[u8]) {
        if let Some(exchange) = self.observer.feed(bytes) {
            tracing::debug!(host = %self.host, "ssh key exchange seen on a spliced connection");
            self.id = Some(self.agents.add(&self.workspace, &self.host, exchange));
        }
    }
}

impl Drop for KexWatch {
    fn drop(&mut self) {
        if let Some(id) = self.id {
            self.agents.remove(id);
        }
    }
}

impl std::fmt::Debug for KexWatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KexWatch")
            .field("host", &self.host)
            .finish_non_exhaustive()
    }
}

/// What the filter of one workspace's stream asks.
struct StreamContext {
    agents: Arc<SshAgents>,
    workspace: WorkspaceId,
    hosts: HashMap<Host, AllowedKeys>,
}

impl Context for StreamContext {
    fn seen(&self, host_key: &KeyData, signature: &[u8]) -> Option<String> {
        self.agents
            .find(&self.workspace, host_key, signature)
            .map(|h| h.to_string())
    }

    fn allowed(&self, host: &str) -> Option<AllowedKeys> {
        self.hosts
            .iter()
            .find(|(h, _)| h.to_string() == host)
            .map(|(_, keys)| keys.clone())
    }

    fn list(&self) -> Result<Vec<Identity>, AgentError> {
        self.agents.agent.list()
    }

    fn sign(&self, key: &[u8], data: &[u8], flags: u32) -> Result<Vec<u8>, AgentError> {
        self.agents.agent.sign(key, data, flags)
    }
}

/// Answers one guest agent stream until the guest closes it, sends something that is not agent
/// protocol framing, or stays silent for [`IDLE_LIMIT`].
pub(crate) async fn serve<S: AsyncRead + AsyncWrite + Unpin>(
    agents: Arc<SshAgents>,
    terminations: Option<Arc<dyn TerminationSource>>,
    workspace: WorkspaceId,
    mut stream: S,
    idle: Duration,
) {
    let mut filter = Filter::new();
    loop {
        let message = match tokio::time::timeout(idle, read_message(&mut stream)).await {
            Ok(Ok(Some(message))) => message,
            Ok(Ok(None)) => return,
            Ok(Err(why)) => {
                tracing::info!(workspace = %workspace, why, "ssh agent stream closed");
                return;
            }
            Err(_) => {
                tracing::info!(workspace = %workspace, idle_secs = idle.as_secs(), "ssh agent stream idle, closed");
                return;
            }
        };
        let hosts = terminations
            .as_ref()
            .and_then(|t| t.termination(&workspace))
            .map(|t| t.ssh_agent_hosts().clone())
            .unwrap_or_default();
        let cx = StreamContext {
            agents: Arc::clone(&agents),
            workspace: workspace.clone(),
            hosts,
        };
        let answered = tokio::task::spawn_blocking(move || {
            let (answer, outcome) = filter.answer(&message, &cx);
            (filter, answer, outcome)
        })
        .await;
        let Ok((back, answer, outcome)) = answered else {
            tracing::warn!(workspace = %workspace, "ssh agent filter panicked; stream closed");
            return;
        };
        filter = back;
        log(&workspace, filter.host(), &outcome);
        if stream.write_all(&answer).await.is_err() {
            return;
        }
    }
}

fn log(workspace: &WorkspaceId, host: Option<&str>, outcome: &Outcome) {
    let host = host.unwrap_or("-");
    match outcome {
        Outcome::Refused(why) => {
            tracing::info!(workspace = %workspace, host, refused = %why, "ssh agent request refused");
        }
        Outcome::Bound(_) => tracing::info!(workspace = %workspace, host, "ssh agent bound"),
        Outcome::Listed(keys) => {
            tracing::debug!(workspace = %workspace, host, keys, "ssh agent keys listed");
        }
        Outcome::Signed(family) => {
            tracing::info!(workspace = %workspace, host, key = family, "ssh agent signed a login");
        }
    }
}

/// Reads one framed message. `Ok(None)`: the stream ended between messages.
async fn read_message<S: AsyncRead + Unpin>(
    stream: &mut S,
) -> Result<Option<Vec<u8>>, &'static str> {
    let mut len = [0_u8; 4];
    if stream.read_exact(&mut len).await.is_err() {
        // The guest closed (or the stream broke) between two messages: nothing was cut short.
        return Ok(None);
    }
    let len = u32::from_be_bytes(len) as usize;
    if len == 0 || len > MAX_MESSAGE {
        return Err("a message length out of range");
    }
    let mut message = vec![0_u8; len];
    stream
        .read_exact(&mut message)
        .await
        .map_err(|_| "the stream broke inside a message")?;
    Ok(Some(message))
}

#[cfg(test)]
mod tests;
