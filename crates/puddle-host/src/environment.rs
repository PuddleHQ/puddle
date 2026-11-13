// SPDX-License-Identifier: AGPL-3.0-or-later
//! A workspace's environment as the host applies it: the variables its guest starts with, and the
//! stand-ins the proxy swaps for the secrets among them.
//!
//! The plain variables and each secret's stand-in go into the guest's environment. A secret's
//! real value is read from the operating system's credential store into the proxy's registry and
//! nowhere else: this module never puts it in the guest's environment, a file or a log line.

use puddle_proxy::{
    Placement, SecretValue, StandIn, StandInOrigin, TerminationSet, secret_stand_in,
};
use puddle_secrets::SecretStore;
use puddle_store::{SecretPlace, StartValue, Store};
use puddle_types::{GuestEnv, WorkspaceId};

/// What a workspace's environment comes to at one moment.
pub(crate) struct Resolved {
    /// What the guest starts with: every plain variable, and a stand-in under each secret's name.
    pub(crate) guest: GuestEnv,
    /// One entry for each secret, with its real value, for the proxy's registry.
    pub(crate) entries: Vec<StandIn>,
}

impl std::fmt::Debug for Resolved {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Resolved")
            .field("variables", &self.guest.len())
            .field("secrets", &self.entries.len())
            .finish()
    }
}

/// Reads `workspace`'s variables from the store and its secrets' values from `vault`. Blocking:
/// the credential store may take its time (a locked keyring), so callers run it off the async
/// threads.
///
/// # Errors
/// A message for the user that names the variable and the way out. A secret whose value cannot be
/// read stops the start: a workspace that started without it would only fail later in a tool, with
/// no hint of the cause.
pub(crate) fn resolve(
    store: &Store,
    vault: &dyn SecretStore,
    workspace: &WorkspaceId,
) -> Result<Resolved, String> {
    let vars = store
        .env_for_start(workspace, &mut |name| {
            secret_stand_in(name.as_str()).map_err(|e| e.to_string())
        })
        .map_err(|e| format!("cannot read {workspace}'s environment: {e}"))?;
    let mut guest = GuestEnv::new();
    let mut entries = Vec::new();
    for var in vars {
        let name = var.name.as_str();
        let text = match var.value {
            StartValue::Plain(value) => value,
            StartValue::Secret {
                id,
                hosts,
                place,
                stand_in,
            } => {
                let real = match vault.get(&id) {
                    Ok(Some(real)) => real,
                    Ok(None) => {
                        return Err(format!(
                            "the value of the secret {name} is not in this computer's credential store; \
                             set it again (the workspace's Environment tab, or Settings for a global secret)"
                        ));
                    }
                    Err(_) => {
                        return Err(format!(
                            "cannot read the secret {name} from the operating system's credential store; \
                             unlock it or check that it is running, then start the workspace again"
                        ));
                    }
                };
                let set = TerminationSet::parse(hosts.iter().map(puddle_store::SecretHost::as_str))
                    .map_err(|e| format!("the hosts of the secret {name} are not usable: {e}"))?;
                let entry = StandIn::new(
                    StandInOrigin::Secret,
                    name,
                    &stand_in,
                    SecretValue::new(real.expose()),
                    set,
                )
                .map(|entry| entry.placed(placement(place)))
                .map_err(|e| format!("the secret {name} cannot be used: {e}"))?;
                entries.push(entry);
                stand_in
            }
        };
        guest
            .set(name, &text)
            .map_err(|e| format!("the variable {name} cannot be set: {e}"))?;
    }
    Ok(Resolved { guest, entries })
}

/// The proxy's placement for a place the store kept. A query name is the same type on both sides,
/// so a name the store refuses cannot reach the proxy.
fn placement(place: SecretPlace) -> Placement {
    match place {
        SecretPlace::Header => Placement::Header,
        SecretPlace::Path => Placement::Path,
        SecretPlace::Query(name) => Placement::Query(name),
    }
}

#[cfg(test)]
mod tests;
