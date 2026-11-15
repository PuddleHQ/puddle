// SPDX-License-Identifier: AGPL-3.0-or-later
//! Who `ssh -T` to a Git host is greeted as: the account of the identity Puddle would use there.
//!
//! The identity is the one on the workspace whose credential for the host covers the rest of the
//! host, else the first on the workspace's list with a credential there. Its account's name is
//! what the host says the credential is (the read an identity's profile makes); when that read
//! fails the greeting goes without a name.

use std::sync::{Arc, OnceLock};

use puddle_proxy::{BoxFuture, GitSshAccount, GitSshAccounts, TerminationSet};
use puddle_repos::Repos;
use puddle_secrets::SourceSpec;
use puddle_store::{Identity, Store};
use puddle_types::{Host, WorkspaceId};

use crate::git_hosts::decrypt_patterns;

/// Names the accounts of a store's identities. The repository reader is given once it exists:
/// the proxy that asks is built before it.
#[derive(Debug)]
pub(crate) struct SshAccounts {
    store: Arc<Store>,
    repos: OnceLock<Arc<Repos>>,
}

impl SshAccounts {
    pub(crate) fn new(store: Arc<Store>) -> Self {
        Self {
            store,
            repos: OnceLock::new(),
        }
    }

    /// Reads account names with `repos` from now on.
    pub(crate) fn read_with(&self, repos: Arc<Repos>) {
        let _ = self.repos.set(repos);
    }
}

impl GitSshAccounts for SshAccounts {
    fn account<'a>(
        &'a self,
        workspace: &'a WorkspaceId,
        host: &'a Host,
    ) -> BoxFuture<'a, GitSshAccount> {
        Box::pin(async move {
            let store = Arc::clone(&self.store);
            let ws = workspace.clone();
            // The store is SQLite: read it off the async threads. A store that cannot be read
            // lists no identity, as the injector then adds no credential either.
            let identities =
                tokio::task::spawn_blocking(move || store.workspace_git(&ws).map(|g| g.identities))
                    .await
                    .ok()
                    .and_then(Result::ok)
                    .unwrap_or_default();
            let Some(source) = credential_for(&identities, host) else {
                return GitSshAccount::NoIdentity;
            };
            let Some(repos) = self.repos.get() else {
                return GitSshAccount::Unnamed;
            };
            repos
                .profile(&source)
                .await
                .profile
                .account
                .map_or(GitSshAccount::Unnamed, GitSshAccount::Named)
        })
    }
}

/// The credential for `host` of the identity `ssh -T` answers as: the first that covers the rest
/// of the host, else the first on the list.
fn credential_for(identities: &[Identity], host: &Host) -> Option<SourceSpec> {
    let on_host = || {
        identities
            .iter()
            .flat_map(|identity| &identity.credentials)
            .filter(|credential| {
                let mut names = TerminationSet::new();
                for pattern in decrypt_patterns(credential.host.as_str()) {
                    // `insert` reports only whether the pattern was new
                    let _ = names.insert(&pattern);
                }
                names.contains(host)
            })
    };
    on_host()
        .find(|credential| credential.covers.rest_of_host)
        .or_else(|| on_host().next())
        .map(|credential| credential.source.clone())
}

#[cfg(test)]
mod tests;
