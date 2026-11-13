// SPDX-License-Identifier: GPL-3.0-or-later
//! The host's side of the filtered SSH agent: which agent on this machine it asks, and which hosts
//! each workspace may use it for.
//!
//! The agent is looked up again for every request, from the settings (`ui.ssh_agent_pipe`) or, with
//! none named, the platform default (the OpenSSH pipe on Windows, this process's `SSH_AUTH_SOCK`
//! elsewhere), so a changed setting applies to the next login with no restart. The hosts come from
//! the store, where only the user's own change puts one.

use std::collections::HashMap;
use std::io;
use std::sync::Arc;

use puddle_api::SettingsRepo;
use puddle_proxy::ssh_agent::{AgentAt, HostAgent};
use puddle_ssh_agent::filter::AllowedKeys;
use puddle_ssh_agent::{AgentEndpoint, AgentError, Identity};
use puddle_store::SshAgentHost;
use puddle_types::Host;

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
