// SPDX-License-Identifier: GPL-3.0-or-later
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
mod tests {
    use std::collections::BTreeSet;

    use puddle_repos::{ApiReply, FakeApi, Secrets};
    use puddle_secrets::{Credential, HostName, Secret, SourceError, StoredId, TokenScope};
    use puddle_store::{
        Author, Coverage, CredentialBinding, IdentityDraft, Limits, ManualClock, Owner,
    };

    use super::*;

    struct OneToken;

    impl Secrets for OneToken {
        fn get<'a>(
            &'a self,
            _spec: &'a SourceSpec,
        ) -> futures_util::future::BoxFuture<'a, Result<Credential, SourceError>> {
            Box::pin(async {
                Ok(Credential {
                    username: None,
                    secret: Arc::new(Secret::new("CANARY-ssh-accounts".to_owned())),
                })
            })
        }

        fn invalidate(&self, _spec: &SourceSpec) {}
    }

    fn source(id: &str, host: &str) -> SourceSpec {
        let host = HostName::new(host).unwrap();
        SourceSpec::Stored {
            id: StoredId::new(id).unwrap(),
            scope: TokenScope { host, org: None },
        }
    }

    /// A store with workspace `box` holding identities made of `(label, host, rest of host)`.
    fn store_with(identities: &[(&str, &str, bool)]) -> (Arc<Store>, Vec<SourceSpec>) {
        let clock = Arc::new(ManualClock::new(1_700_000_000_000));
        let store = Arc::new(Store::open_in_memory(clock, Limits::default()).unwrap());
        let ws = WorkspaceId::new("box").unwrap();
        let mut sources = Vec::new();
        for (label, host, rest) in identities {
            let spec = source(&format!("{label}tok"), host);
            let owners = if *rest {
                BTreeSet::new()
            } else {
                BTreeSet::from([Owner::new(label).unwrap()])
            };
            let identity = store
                .create_identity(IdentityDraft {
                    label: (*label).to_owned(),
                    author: Author::new(label, "a@example.org").unwrap(),
                    credentials: vec![
                        CredentialBinding::new(
                            &HostName::new(*host).unwrap(),
                            spec.clone(),
                            Coverage::new(owners, *rest).unwrap(),
                        )
                        .unwrap(),
                    ],
                })
                .unwrap();
            store.attach_identity(&ws, identity.id, None).unwrap();
            sources.push(spec);
        }
        (store, sources)
    }

    fn host(name: &str) -> Host {
        Host::parse_normalised(name).unwrap()
    }

    #[test]
    fn the_identity_that_covers_the_rest_of_the_host_comes_before_the_first_on_the_list() {
        let (store, sources) = store_with(&[
            ("acme", "github.com", false),
            ("other", "git.example.org", true),
            ("me", "github.com", true),
        ]);
        let git = store
            .workspace_git(&WorkspaceId::new("box").unwrap())
            .unwrap();
        assert_eq!(
            credential_for(&git.identities, &host("github.com")),
            Some(sources[2].clone())
        );
        assert_eq!(
            credential_for(&git.identities, &host("git.example.org")),
            Some(sources[1].clone())
        );
        assert_eq!(credential_for(&git.identities, &host("gitlab.com")), None);
    }

    #[test]
    fn with_none_covering_the_rest_the_first_on_the_list_is_used() {
        let (store, sources) = store_with(&[
            ("acme", "dev.azure.com", false),
            ("beta", "dev.azure.com", false),
        ]);
        let git = store
            .workspace_git(&WorkspaceId::new("box").unwrap())
            .unwrap();
        assert_eq!(
            credential_for(&git.identities, &host("dev.azure.com")),
            Some(sources[0].clone())
        );
        // The organisation names of Azure DevOps are the same credential's.
        assert_eq!(
            credential_for(&git.identities, &host("acme.visualstudio.com")),
            Some(sources[0].clone())
        );
    }

    #[tokio::test]
    async fn the_account_is_named_by_what_the_host_says_the_credential_is() {
        let (store, _) = store_with(&[("me", "github.com", true)]);
        let accounts = SshAccounts::new(store);
        let ws = WorkspaceId::new("box").unwrap();
        // Before the reader is given there is no name to read.
        assert_eq!(
            accounts.account(&ws, &host("github.com")).await,
            GitSshAccount::Unnamed
        );
        let api = Arc::new(FakeApi::new());
        api.reply(
            "api.github.com",
            "/user",
            ApiReply::new(200, r#"{"login":"octocat","id":1,"name":null}"#),
        );
        api.reply(
            "api.github.com",
            "/user/orgs?per_page=100",
            ApiReply::new(200, "[]"),
        );
        let clock = Arc::new(ManualClock::new(1_700_000_000_000));
        accounts.read_with(Arc::new(Repos::new(api, Arc::new(OneToken), clock)));
        assert_eq!(
            accounts.account(&ws, &host("github.com")).await,
            GitSshAccount::Named("octocat".to_owned())
        );
        assert_eq!(
            accounts.account(&ws, &host("gitlab.com")).await,
            GitSshAccount::NoIdentity
        );
    }

    #[tokio::test]
    async fn a_profile_that_cannot_be_read_greets_without_a_name() {
        let (store, _) = store_with(&[("me", "github.com", true)]);
        let accounts = SshAccounts::new(store);
        let clock = Arc::new(ManualClock::new(1_700_000_000_000));
        // The fake store of secrets is never told a credential went stale here.
        OneToken.invalidate(&source("unused", "github.com"));
        let api = Arc::new(FakeApi::new());
        api.reply("api.github.com", "/user", ApiReply::new(401, "{}"));
        accounts.read_with(Arc::new(Repos::new(api, Arc::new(OneToken), clock)));
        assert_eq!(
            accounts
                .account(&WorkspaceId::new("box").unwrap(), &host("github.com"))
                .await,
            GitSshAccount::Unnamed
        );
    }
}
