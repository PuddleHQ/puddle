// SPDX-License-Identifier: AGPL-3.0-or-later
//! The closed list of places a secret can come from.

use serde::{Deserialize, Serialize};

use crate::error::SourceError;
use crate::name::{AccountName, HostName, OrgName, StoredId, TenantId, UrlPath};

/// The host an Azure CLI token is for. `*.visualstudio.com` names are the same service.
pub const AZURE_DEVOPS_HOST: &str = "dev.azure.com";

/// Which host (and, for a token that belongs to one organisation, which organisation) a secret is
/// for. Sources do not enforce it: whoever uses the token (the proxy's injector, a host-side API
/// call) checks it against where the token is about to go.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TokenScope {
    /// The Git host.
    pub host: HostName,
    /// The organisation, for tokens that are per organisation (Azure DevOps).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub org: Option<OrgName>,
}

/// One secret source. A closed enum: there is no variant that carries a command line, so no
/// configuration or API input can make Puddle run anything but the fixed invocations in this crate.
/// An identity owns one of these per credential; a source never assumes it is the only one for a
/// workspace.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum SourceSpec {
    /// The token of a named, signed-in GitHub CLI account (`gh auth token --user`), never "whichever
    /// account is active".
    Gh {
        /// The GitHub host.
        host: HostName,
        /// The account.
        account: AccountName,
    },
    /// Git Credential Manager (or any configured Git credential helper), asked for one HTTPS URL
    /// with its path. The path is always sent.
    GitCredential {
        /// The host.
        host: HostName,
        /// The URL path that selects the credential.
        path: UrlPath,
        /// A GitHub account name, when the host has several.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        username: Option<AccountName>,
    },
    /// A token the user pasted, kept in the OS credential store under Puddle's own name.
    Stored {
        /// The id of the entry.
        id: StoredId,
        /// What the token is for.
        scope: TokenScope,
    },
    /// The Entra access token of the account signed in to the Azure CLI (`az`) for one tenant, for
    /// Azure DevOps only. The user signs in to `az` themselves; Puddle only reads a token. There is
    /// no resource, scope or command field: the resource is Azure DevOps' own application id.
    AzCli {
        /// The tenant to ask `az` for, never "whichever tenant is active".
        tenant: TenantId,
        /// The Azure DevOps organisation, when the token is meant for one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        org: Option<OrgName>,
    },
}

impl SourceSpec {
    /// What the credential is for, when the source knows.
    #[must_use]
    pub fn scope(&self) -> TokenScope {
        match self {
            Self::Gh { host, .. } | Self::GitCredential { host, .. } => TokenScope {
                host: host.clone(),
                org: None,
            },
            Self::Stored { scope, .. } => scope.clone(),
            Self::AzCli { org, .. } => TokenScope {
                host: azure_devops_host(),
                org: org.clone(),
            },
        }
    }

    /// A line for logs, events and errors. Names only, never a value.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::Gh { host, account } => format!("gh account {account} on {host}"),
            Self::GitCredential {
                host,
                path,
                username: None,
            } => {
                format!("Git credential for https://{host}/{path}")
            }
            Self::GitCredential {
                host,
                path,
                username: Some(user),
            } => {
                format!("Git credential for https://{host}/{path} (account {user})")
            }
            Self::Stored { id, scope } => format!("stored token {id} for {}", scope.host),
            Self::AzCli { tenant, org: None } => format!("Azure CLI account in tenant {tenant}"),
            Self::AzCli {
                tenant,
                org: Some(org),
            } => format!("Azure CLI account in tenant {tenant} for {org}"),
        }
    }

    /// The exact step that fixes `err` for this source, when there is one the user can take
    /// (only the Azure CLI has any: nothing there starts a sign-in for the user).
    #[must_use]
    pub fn fix_step(&self, err: &SourceError) -> Option<String> {
        let Self::AzCli { tenant, org } = self else {
            return None;
        };
        let on = org
            .as_ref()
            .map_or_else(String::new, |o| format!(" for {o}"));
        match err {
            SourceError::ToolMissing(_) => Some(
                "Install the Azure CLI (https://aka.ms/installazurecli), then open a new terminal so `az` is on the PATH.".to_owned(),
            ),
            SourceError::NotSignedIn => Some(format!(
                "Run `az login --tenant {tenant}` in a terminal and finish the sign-in{on}."
            )),
            SourceError::WrongTenant => Some(format!(
                "The signed-in account is not in tenant {tenant}. Run `az login --tenant {tenant}` with an account that is a member of it."
            )),
            SourceError::NoAccess => Some(match org {
                Some(org) => format!(
                    "The account has no access to Azure DevOps in tenant {tenant}. Ask an administrator of the organisation {org} to add it, then try again."
                ),
                None => format!(
                    "The account has no access to Azure DevOps in tenant {tenant}. Ask an organisation administrator to add it, then try again."
                ),
            }),
            SourceError::Timeout(_) => Some(
                "`az` did not answer in time. Run `az account show` in a terminal to see whether it is waiting for something.".to_owned(),
            ),
            _ => None,
        }
    }
}

#[expect(
    clippy::expect_used,
    reason = "a constant that is a valid host name; the test below proves it"
)]
fn azure_devops_host() -> HostName {
    HostName::new(AZURE_DEVOPS_HOST).expect("a valid host name")
}

#[cfg(test)]
mod tests;
